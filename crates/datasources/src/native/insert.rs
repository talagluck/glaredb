use std::any::Any;
use std::sync::Arc;

use datafusion::arrow::datatypes::SchemaRef;
use datafusion::error::{DataFusionError, Result as DataFusionResult};
use datafusion::execution::context::SessionState;
use datafusion::execution::TaskContext;
use datafusion::physical_expr::PhysicalSortExpr;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs,
    DisplayFormatType,
    Distribution,
    ExecutionPlan,
    Partitioning,
    SendableRecordBatchStream,
    Statistics,
};
use deltalake::logstore::LogStore;
use deltalake::operations::write::WriteBuilder;
use deltalake::protocol::SaveMode;
use deltalake::table::state::DeltaTableState;
use futures::StreamExt;

use crate::common::util::{create_count_record_batch, COUNT_SCHEMA};

/// An execution plan for inserting data into a delta table.
#[derive(Debug)]
pub struct NativeTableInsertExec {
    input: Arc<dyn ExecutionPlan>,
    store: Arc<dyn LogStore>,
    snapshot: DeltaTableState,
    save_mode: SaveMode,
}

impl NativeTableInsertExec {
    pub fn new(
        input: Arc<dyn ExecutionPlan>,
        store: Arc<dyn LogStore>,
        snapshot: DeltaTableState,
        save_mode: SaveMode,
    ) -> Self {
        NativeTableInsertExec {
            input,
            store,
            snapshot,
            save_mode,
        }
    }
}

impl ExecutionPlan for NativeTableInsertExec {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        COUNT_SCHEMA.clone()
    }

    fn output_partitioning(&self) -> Partitioning {
        Partitioning::UnknownPartitioning(1)
    }

    fn output_ordering(&self) -> Option<&[PhysicalSortExpr]> {
        None
    }

    fn required_input_distribution(&self) -> Vec<Distribution> {
        vec![Distribution::UnspecifiedDistribution]
    }

    fn maintains_input_order(&self) -> Vec<bool> {
        vec![false]
    }

    fn children(&self) -> Vec<Arc<dyn ExecutionPlan>> {
        vec![self.input.clone()]
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> DataFusionResult<Arc<dyn ExecutionPlan>> {
        Ok(Arc::new(Self {
            input: children[0].clone(),
            store: self.store.clone(),
            snapshot: self.snapshot.clone(),
            save_mode: self.save_mode.clone(),
        }))
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> DataFusionResult<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(DataFusionError::Internal(
                format!("Invalid requested partition {partition}. NativeTableInsertExec requires a single input partition.")));
        }

        // This is needed since we might be inserting from a plan that includes
        // a client recv exec. That exec requires that we have an appropriate
        // set of extensions.
        let state = SessionState::new_with_config_rt(
            context.session_config().clone(),
            context.runtime_env(),
        );
        // Allows writing multiple output partitions from the input execution
        // plan.
        //
        // TODO: Possibly try avoiding cloning the snapshot.
        let builder = WriteBuilder::new(self.store.clone(), Some(self.snapshot.clone()))
            .with_input_session_state(state)
            .with_save_mode(self.save_mode.clone())
            .with_input_execution_plan(self.input.clone());

        let input = self.input.clone();
        let output = futures::stream::once(async move {
            let _ = builder
                .await
                .map_err(|e| DataFusionError::External(Box::new(e)))?;

            let count = input
                .metrics()
                .map(|metrics| metrics.output_rows().unwrap_or_default())
                .unwrap_or_default();

            Ok(create_count_record_batch(count as u64))
        })
        .boxed();

        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.schema(),
            output,
        )))
    }

    fn statistics(&self) -> DataFusionResult<Statistics> {
        Ok(Statistics::new_unknown(self.schema().as_ref()))
    }
}

impl DisplayAs for NativeTableInsertExec {
    fn fmt_as(&self, t: DisplayFormatType, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match t {
            DisplayFormatType::Default => {
                write!(f, "NativeTableInsertExec")
            }
            DisplayFormatType::Verbose => {
                write!(f, "NativeTableInsertExec")
            }
        }
    }
}
