use crate::errors::{internal, Result};
use crate::logical_plan::*;
use crate::parser::{CustomParser, StatementWithExtensions};
use crate::planner::SessionPlanner;
use crate::session::Session;
use datafusion::physical_plan::SendableRecordBatchStream;
use std::collections::VecDeque;
use std::fmt;

/// Results from a sql statement execution.
pub enum ExecutionResult {
    /// The stream for the output of a query.
    Query { stream: SendableRecordBatchStream },
    /// Transaction started.
    Begin,
    /// Transaction committed,
    Commit,
    /// Transaction rolled abck.
    Rollback,
    /// Data successfully written.
    WriteSuccess,
    /// Table created.
    CreateTable,
    /// Schema created.
    CreateSchema,
    /// A client local variable was set.
    SetLocal,
    /// Tables dropped.
    DropTables,
    /// Schemas dropped.
    DropSchemas,
}

impl fmt::Debug for ExecutionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionResult::Query { stream } => write!(f, "query (schema: {:?})", stream.schema()),
            ExecutionResult::Begin => write!(f, "begin"),
            ExecutionResult::Commit => write!(f, "commit"),
            ExecutionResult::Rollback => write!(f, "rollback"),
            ExecutionResult::WriteSuccess => write!(f, "write success"),
            ExecutionResult::CreateTable => write!(f, "create table"),
            ExecutionResult::CreateSchema => write!(f, "create schema"),
            ExecutionResult::SetLocal => write!(f, "set local"),
            ExecutionResult::DropTables => write!(f, "drop tables"),
            ExecutionResult::DropSchemas => write!(f, "drop schemas"),
        }
    }
}

/// A thin wrapper around a session responsible for pull-based execution for a
/// sql statement.
///
/// The underlying session will go through the following phases on every call to
/// "next".
/// - Logical planning and optimization
/// - Physical query execution
///
/// Depending on the type of query being executed, the execution result itself
/// may also contains a stream. If the caller does not consume the returned
/// stream, there are no guarantees about the results of any of the following
/// executions.
pub struct Executor<'a> {
    /// All parsed statements.
    statements: VecDeque<StatementWithExtensions>,
    session: &'a mut Session,
}

impl<'a> Executor<'a> {
    /// Create a new executor with the provided sql string and session.
    pub fn new(sql: &'a str, session: &'a mut Session) -> Result<Self> {
        let statements = CustomParser::parse_sql(sql)?;
        // TODO: Implicit transaction.
        Ok(Executor {
            statements,
            session,
        })
    }

    pub fn statements_remaining(&self) -> usize {
        self.statements.len()
    }

    /// Execute the next statement.
    ///
    /// Returns `None` if there's no more statements to execute.
    pub async fn execute_next(&mut self) -> Option<Result<ExecutionResult>> {
        let statement = self.statements.pop_front()?;
        Some(self.execute_statement(statement).await)
    }

    async fn execute_statement(
        &mut self,
        stmt: StatementWithExtensions,
    ) -> Result<ExecutionResult> {
        let plan = {
            let planner = SessionPlanner::new(&self.session.ctx);
            planner.plan_ast(stmt)?
        };
        match plan {
            LogicalPlan::Ddl(DdlPlan::CreateTable(plan)) => {
                self.session.create_table(plan).await?;
                Ok(ExecutionResult::CreateTable)
            }
            LogicalPlan::Ddl(DdlPlan::CreateExternalTable(plan)) => {
                self.session.create_external_table(plan).await?;
                Ok(ExecutionResult::CreateTable)
            }
            LogicalPlan::Ddl(DdlPlan::CreateTableAs(plan)) => {
                self.session.create_table_as(plan).await?;
                Ok(ExecutionResult::CreateTable)
            }
            LogicalPlan::Ddl(DdlPlan::CreateSchema(plan)) => {
                self.session.create_schema(plan).await?;
                Ok(ExecutionResult::CreateSchema)
            }
            LogicalPlan::Ddl(DdlPlan::DropTables(plan)) => {
                self.session.drop_tables(plan).await?;
                Ok(ExecutionResult::DropTables)
            }
            LogicalPlan::Ddl(DdlPlan::DropSchemas(plan)) => {
                self.session.drop_schemas(plan).await?;
                Ok(ExecutionResult::DropSchemas)
            }
            LogicalPlan::Write(WritePlan::Insert(plan)) => {
                self.session.insert(plan).await?;
                Ok(ExecutionResult::WriteSuccess)
            }
            LogicalPlan::Query(plan) => {
                let physical = self.session.create_physical_plan(plan).await?;
                let stream = self.session.execute_physical(physical)?;
                Ok(ExecutionResult::Query { stream })
            }
            LogicalPlan::Configuration(ConfigurationPlan::SetConfiguration(plan)) => {
                self.session.set_configuration(plan)?;
                Ok(ExecutionResult::SetLocal)
            }
            other => Err(internal!("unimplemented logical plan: {:?}", other)),
        }
    }
}
