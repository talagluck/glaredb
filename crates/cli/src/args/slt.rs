use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::Args;
use pgsrv::auth::SingleUserAuthenticator;
use slt::clients::flightsql::FlightSqlTestClient;
use slt::clients::postgres::PgTestClient;
use slt::clients::rpc::RpcTestClient;
use slt::clients::{ClientProtocol, TestClient};
use slt::test::{Test, TestHooks};
use tokio::net::TcpListener;
use tokio::runtime::Builder;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_postgres::config::Config as ClientConfig;
use uuid::Uuid;

use crate::args::StorageConfigArgs;
use crate::server::ComputeServer;

#[derive(Args)]
pub struct SltArgs {
    /// TCP address to bind to for the GlareDB server.
    ///
    /// Omitting this will attempt to bind to any available port.
    #[arg(long, value_parser)]
    bind_embedded: Option<String>,

    /// Address of metastore to use.
    ///
    /// If not provided, a Metastore will be spun up automatically.
    #[arg(long, value_parser)]
    metastore_addr: Option<String>,

    /// Whether or not to keep the embedded GlareDB server running after a
    /// failure.
    ///
    /// This allow for an external client to connect to allow for additional
    /// debugging.
    #[arg(long, value_parser)]
    keep_running: bool,

    /// Connection string to use for connecting to the database.
    ///
    /// If provided, an embedded server won't be started.
    #[arg(short, long, value_parser)]
    connection_string: Option<String>,

    /// List all the tests for the pattern (Dry Run).
    #[arg(long, value_parser)]
    list: bool,

    /// Number of jobs to run in parallel
    ///
    /// To run the max possible jobs, set it to 0. By default, this argument is
    /// set to 0 to run max possible jobs. Set it to `1` to run sequentially.
    #[arg(short, long, value_parser, default_value_t = 0)]
    jobs: u8,

    /// Timeout (exit) after this number of seconds.
    #[arg(long, value_parser, default_value_t = 5 * 60)]
    timeout: u64,

    /// Exclude these tests from the run.
    #[arg(short, long, value_parser)]
    exclude: Vec<String>,

    /// Client protocol to use. (rpc, postgres, flightsql)
    #[arg(long, short, value_enum, default_value_t=ClientProtocol::Postgres)]
    protocol: ClientProtocol,

    #[command(flatten)]
    storage_config: StorageConfigArgs,

    /// Tests to run.
    ///
    /// Provide glob like regexes for test names. If omitted, runs all the
    /// tests. This is similar to providing parameter as `*`.
    #[arg(value_parser)]
    tests_pattern: Option<Vec<String>>,
}

impl SltArgs {
    pub fn execute(&self, tests: BTreeMap<String, Test>, hooks: TestHooks) -> Result<()> {
        let tests = self.collect_tests(tests)?;

        if self.list {
            for (test_name, _) in tests {
                println!("{test_name}");
            }
            return Ok(());
        }

        // Abort the program on panic. This will ensure that slt tests will
        // never pass if there's a panic somewhere.
        std::panic::set_hook(Box::new(|info| {
            let backtrace = std::backtrace::Backtrace::force_capture();
            println!("Info: {}\n\nBacktrace:{}", info, backtrace);
            std::process::abort();
        }));

        Builder::new_multi_thread()
            .enable_all()
            // Bump the stack from th default 2MB.
            //
            // We reach the limit when planning a query in an SLT where we have
            // a nested view. The 4MB allows that test to pass.
            //
            // Note that Sean observed the stack size only reaching ~300KB when
            // running in release mode, and so we don't need to bump this
            // everywhere. However there's definitely improvements to stack
            // usage that we can make.
            .thread_stack_size(4 * 1024 * 1024)
            .build()?
            .block_on(async move {
                let batch_size = num_cpus::get();
                tracing::trace!(%batch_size, "test batch size");
                self.run_tests_batched(batch_size, tests, hooks).await
            })
    }

    fn collect_tests(&self, tests: BTreeMap<String, Test>) -> Result<Vec<(String, Test)>> {
        let mut tests: Vec<_> = if let Some(patterns) = &self.tests_pattern {
            let patterns = patterns
                .iter()
                .map(|p| p.trim_end_matches(".slt"))
                .map(glob::Pattern::new)
                .collect::<Result<Vec<_>, _>>()?;

            tests
                .into_iter()
                .filter(|(k, _v)| patterns.iter().any(|p| p.matches(k)))
                .collect()
        } else {
            tests.into_iter().collect()
        };
        // See if we want to exclude anything
        for pattern in &self.exclude {
            let pattern = glob::Pattern::new(pattern)
                .map_err(|e| anyhow!("Invalid glob pattern `{pattern}`: {e}"))?;
            tests.retain(|(k, _v)| !pattern.matches(k));
        }

        if tests.is_empty() {
            return Err(anyhow!("No tests to run. Exiting..."));
        }

        Ok(tests)
    }

    /// Run all provided tests, in batches of size `batch_size`.
    ///
    /// Batches will be ran sequentially, and an error resulting from a batch
    /// will halt further execution.
    async fn run_tests_batched(
        &self,
        batch_size: usize,
        mut tests: Vec<(String, Test)>,
        hooks: TestHooks,
    ) -> Result<()> {
        // Temp directory for metastore
        let temp_dir = tempfile::tempdir()?;

        let configs: HashMap<String, ClientConfig> =
            if let Some(connection_string) = &self.connection_string {
                let config: ClientConfig = connection_string.parse()?;
                let mut configs = HashMap::with_capacity(tests.len());
                tests.iter().for_each(|(name, _)| {
                    configs.insert(name.clone(), config.clone());
                });
                configs
            } else {
                let bind_addr = self
                    .bind_embedded
                    .clone()
                    .unwrap_or_else(|| "0.0.0.0:0".to_string());

                let (pg_listener, rpc_listener, socket_addr) = match self.protocol {
                    ClientProtocol::Postgres => {
                        let listener = TcpListener::bind(bind_addr.clone()).await?;
                        let addr = listener.local_addr().unwrap();
                        (Some(listener), None, addr)
                    }
                    ClientProtocol::Rpc | ClientProtocol::FlightSql => {
                        let listener = TcpListener::bind(bind_addr.clone()).await?;
                        let addr = listener.local_addr().unwrap();
                        (None, Some(listener), addr)
                    }
                };

                let mut builder = ComputeServer::builder()
                    .with_authenticator(SingleUserAuthenticator {
                        user: "glaredb".to_string(),
                        password: "glaredb".to_string(),
                    })
                    .with_pg_listener_opt(pg_listener)
                    .with_rpc_listener_opt(rpc_listener)
                    .with_data_dir(temp_dir.path().to_path_buf())
                    .with_location_opt(self.storage_config.location.clone())
                    .with_storage_options(HashMap::from_iter(
                        self.storage_config.storage_options.clone(),
                    ))
                    .integration_testing_mode(true)
                    .enable_flight_api(true);

                if matches!(self.protocol, ClientProtocol::Rpc) {
                    builder = builder.disable_rpc_auth(true);
                }

                let server = builder.connect().await?;

                tokio::spawn(server.serve());

                let mut configs = HashMap::new();
                let host = socket_addr.ip().to_string();
                let port = socket_addr.port();
                let mut config = ClientConfig::new();
                config
                    .user("glaredb")
                    .password("glaredb")
                    .dbname("glaredb")
                    .host(&host)
                    .port(port);

                tests.iter().for_each(|(name, _)| {
                    let mut cfg = config.clone();
                    let db_id = Uuid::new_v4().to_string();
                    cfg.dbname(&db_id);
                    configs.insert(name.clone(), cfg);
                });

                configs
            };

        // Break up into batches.
        //
        // Rust doesn't have a good way of breaking a Vec into a Vec of Vecs
        // with owned references, so do it manually.
        let mut batches = Vec::new();
        loop {
            let batch: Vec<_> = tests
                .drain(0..usize::min(batch_size, tests.len()))
                .collect();
            if batch.is_empty() {
                break;
            }
            batches.push(batch)
        }

        let start = Instant::now();

        for batch in batches {
            self.run_tests(&configs, batch, hooks.clone(), temp_dir.path())
                .await?;
        }

        let time_taken = Instant::now().duration_since(start);
        eprintln!("Tests took {time_taken:?} to run");

        Ok(())
    }

    async fn run_tests(
        &self,
        configs: &HashMap<String, ClientConfig>,
        tests: Vec<(String, Test)>,
        hooks: TestHooks,
        data_dir: &Path,
    ) -> Result<()> {
        let (jobs_tx, mut jobs_rx) = mpsc::unbounded_channel();
        let mut total_jobs = if self.jobs > 0 { self.jobs } else { u8::MAX };

        let num_tests = tests.len();
        let mut results = Vec::with_capacity(num_tests);

        let timeout_at = Instant::now() + Duration::from_secs(self.timeout);

        type Res = (String, Result<()>);
        async fn recv(
            rx: &mut mpsc::UnboundedReceiver<Res>,
            deadline: Instant,
        ) -> Result<Option<Res>> {
            let res = tokio::time::timeout_at(deadline, rx.recv()).await?;
            Ok(res)
        }

        let hooks = Arc::new(hooks);

        for (test_name, test) in tests {
            if total_jobs == 0 {
                // Wait to receive a result
                let res = recv(&mut jobs_rx, timeout_at).await?.unwrap();
                total_jobs += 1;
                results.push(res);
            }

            // Spawn a new job.
            total_jobs -= 1;
            let cfg = configs.get(&test_name).unwrap().clone();
            let tx = jobs_tx.clone();
            let hooks = Arc::clone(&hooks);

            let protocol = self.protocol;
            let data_dir = data_dir.to_path_buf();

            tokio::spawn(async move {
                let res = Self::run_test(protocol, data_dir, &test_name, test, cfg, hooks).await;
                tx.send((test_name.clone(), res)).unwrap();
            });
        }

        // Drain all the results.
        while let Some(res) = recv(&mut jobs_rx, timeout_at).await? {
            results.push(res);

            // Received everything? Close the channel and exit!
            if results.len() == num_tests {
                jobs_rx.close();
                break;
            }
        }

        let mut errored = false;
        let errors = results.iter().filter_map(|(name, res)| match res {
            Ok(_) => None,
            Err(e) => Some((name, e)),
        });

        for (name, error) in errors {
            errored = true;
            tracing::error!(%error, "Error while running test `{name}`");

            // If keep running, then connect to the client and do it!
            if self.connection_string.is_none() && self.keep_running {
                let conf = configs.get(name).unwrap();
                let port = conf.get_ports().first().unwrap();
                let password = String::from_utf8_lossy(conf.get_password().unwrap()).into_owned();
                let conn_string = format!(
                    "host=localhost port={} dbname={} user={} password={}",
                    port,
                    conf.get_dbname().unwrap(),
                    conf.get_user().unwrap(),
                    password
                );
                println!("connect to the database using connection string:\n  \"{conn_string}\"\n");
            }
        }

        if errored {
            if self.connection_string.is_none() && self.keep_running {
                println!("keeping the server running.");
                println!("connect to the corresponding database using the given connection strings with each error");
                println!("CTRL-C to exit");
                tokio::signal::ctrl_c().await?;
            }
            Err(anyhow!("Test failures"))
        } else {
            Ok(())
        }
    }

    async fn run_test(
        mode: ClientProtocol,
        data_dir: PathBuf,
        test_name: &str,
        test: Test,
        client_config: ClientConfig,
        hooks: Arc<TestHooks>,
    ) -> Result<()> {
        tracing::info!("Running test: `{}`", test_name);
        let client = match mode {
            ClientProtocol::Postgres => TestClient::Pg(PgTestClient::new(&client_config).await?),
            ClientProtocol::Rpc => {
                TestClient::Rpc(RpcTestClient::new(data_dir, &client_config).await?)
            }
            ClientProtocol::FlightSql => {
                TestClient::FlightSql(FlightSqlTestClient::new(&client_config).await?)
            }
        };

        let res = Self::run_test_inner(&client, test_name, test, &client_config, hooks).await;
        // No need to wait for session's close handler since we don't wait for
        // sessions to end in integration testing mode while closing the server.
        let _ = client.close().await;
        res
    }

    async fn run_test_inner(
        client: &TestClient,
        test_name: &str,
        test: Test,
        client_config: &ClientConfig,
        hooks: Arc<TestHooks>,
    ) -> Result<()> {
        let start = Instant::now();

        let mut local_vars = HashMap::new();

        // Run the actual test
        let hooks = hooks
            .iter()
            .filter(|(pattern, _)| pattern.matches(test_name));

        // Run the pre-test hooks
        for (pattern, hook) in hooks.clone() {
            tracing::debug!(%pattern, %test_name, "Running pre hook for test");
            let ok_to_continue = hook
                .pre(client_config, client.clone(), &mut local_vars)
                .await?;
            if !ok_to_continue {
                tracing::warn!("skipping test, as indicated by pre-hook for {}", test_name);
                return Ok(());
            }
        }

        // Run the actual test
        test.execute(client_config, client.clone(), &mut local_vars)
            .await?;

        // Run the post-test hooks
        for (pattern, hook) in hooks {
            tracing::debug!(%pattern, %test_name, "Running post hook for test");
            hook.post(client_config, client.clone(), &local_vars)
                .await?;
        }

        let time_taken = Instant::now().duration_since(start);
        tracing::debug!(?time_taken, %test_name, "Done executing");

        Ok(())
    }
}
