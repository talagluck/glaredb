use super::*;
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct DropCredentials {
    pub names: Vec<String>,
    pub if_exists: bool,
}

impl UserDefinedLogicalNodeCore for DropCredentials {
    fn name(&self) -> &str {
        Self::EXTENSION_NAME
    }

    fn inputs(&self) -> Vec<&DfLogicalPlan> {
        vec![]
    }

    fn schema(&self) -> &datafusion::common::DFSchemaRef {
        &EMPTY_SCHEMA
    }

    fn expressions(&self) -> Vec<datafusion::prelude::Expr> {
        vec![]
    }

    fn fmt_for_explain(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "DropCredentials")
    }

    fn from_template(
        &self,
        _exprs: &[datafusion::prelude::Expr],
        _inputs: &[DfLogicalPlan],
    ) -> Self {
        self.clone()
    }
}

impl ExtensionNode for DropCredentials {
    type ProtoRepr = protogen::sqlexec::logical_plan::DropCredentials;
    const EXTENSION_NAME: &'static str = "DropCredentials";
    fn try_decode(
        proto: Self::ProtoRepr,
        _ctx: &SessionContext,
        _codec: &dyn LogicalExtensionCodec,
    ) -> std::result::Result<Self, ProtoConvError> {
        Ok(Self {
            names: proto.names,
            if_exists: proto.if_exists,
        })
    }
    fn try_decode_extension(extension: &LogicalPlanExtension) -> Result<Self> {
        match extension.node.as_any().downcast_ref::<Self>() {
            Some(s) => Ok(s.clone()),
            None => Err(internal!("DropCredentials::try_decode_extension failed",)),
        }
    }

    fn try_encode(&self, buf: &mut Vec<u8>, _codec: &dyn LogicalExtensionCodec) -> Result<()> {
        use ::protogen::sqlexec::logical_plan::{
            self as protogen, LogicalPlanExtension, LogicalPlanExtensionType,
        };

        let proto = protogen::DropCredentials {
            names: self.names.clone(),
            if_exists: self.if_exists,
        };

        let plan_type = LogicalPlanExtensionType::DropCredentials(proto);

        let lp_extension = LogicalPlanExtension {
            inner: Some(plan_type),
        };

        lp_extension
            .encode(buf)
            .map_err(|e| internal!("{}", e.to_string()))?;

        Ok(())
    }
}