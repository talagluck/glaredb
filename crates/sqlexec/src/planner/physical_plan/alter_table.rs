use std::any::Any;
use std::fmt;
use std::sync::Arc;

use catalog::mutator::CatalogMutator;
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::error::{DataFusionError, Result as DataFusionResult};
use datafusion::execution::TaskContext;
use datafusion::physical_expr::PhysicalSortExpr;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::{
    DisplayAs,
    DisplayFormatType,
    ExecutionPlan,
    Partitioning,
    SendableRecordBatchStream,
    Statistics,
};
use futures::stream;
use protogen::metastore::types::service::{self, AlterTableOperation, Mutation};

use super::{new_operation_batch, GENERIC_OPERATION_PHYSICAL_SCHEMA};

#[derive(Debug, Clone)]
pub struct AlterTableExec {
    pub catalog_version: u64,
    pub schema: String,
    pub name: String,
    pub operation: AlterTableOperation,
}

impl ExecutionPlan for AlterTableExec {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> Arc<Schema> {
        GENERIC_OPERATION_PHYSICAL_SCHEMA.clone()
    }

    fn output_partitioning(&self) -> Partitioning {
        Partitioning::UnknownPartitioning(1)
    }

    fn output_ordering(&self) -> Option<&[PhysicalSortExpr]> {
        None
    }

    fn children(&self) -> Vec<Arc<dyn ExecutionPlan>> {
        Vec::new()
    }

    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> DataFusionResult<Arc<dyn ExecutionPlan>> {
        if children.is_empty() {
            Ok(self)
        } else {
            Err(DataFusionError::Plan(
                "Cannot change children for AlterTableRenameExec".to_string(),
            ))
        }
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> DataFusionResult<SendableRecordBatchStream> {
        if partition != 0 {
            return Err(DataFusionError::Execution(
                "AlterTableExec only supports 1 partition".to_string(),
            ));
        }

        let mutator = context
            .session_config()
            .get_extension::<CatalogMutator>()
            .expect("context should have catalog mutator");

        let stream = stream::once(alter_table_rename(mutator, self.clone()));

        Ok(Box::pin(RecordBatchStreamAdapter::new(
            self.schema(),
            stream,
        )))
    }

    fn statistics(&self) -> DataFusionResult<Statistics> {
        Ok(Statistics::new_unknown(self.schema().as_ref()))
    }
}

impl DisplayAs for AlterTableExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "AlterTableExec")
    }
}

async fn alter_table_rename(
    mutator: Arc<CatalogMutator>,
    plan: AlterTableExec,
) -> DataFusionResult<RecordBatch> {
    // TODO: Error if schemas between references differ.
    mutator
        .mutate_and_commit(
            plan.catalog_version,
            [Mutation::AlterTable(service::AlterTable {
                schema: plan.schema,
                name: plan.name,
                operation: plan.operation,
            })],
        )
        .await
        .map_err(|e| DataFusionError::Execution(format!("failed to alter table: {e}")))?;

    Ok(new_operation_batch("alter_table"))
}
