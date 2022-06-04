use crate::catalog::{Catalog, CatalogError, ResolvedTableReference, TableReference};
use coretypes::{
    datatype::{DataType, DataValue, NullableType, RelationSchema},
    expr::ScalarExpr,
};
use fmtutil::DisplaySlice;
use sqlparser::ast;
use sqlparser::parser::{Parser, ParserError};
use std::fmt;

#[derive(Debug)]
pub enum RelationalPlan {
    /// Evaluate a filter on all inputs.
    ///
    /// "WHERE ..."
    Filter(Filter),
    /// Project from inputs.
    ///
    /// "SELECT ..."
    Project(Project),
    /// Join two plan nodes.
    Join(Join),
    /// Cross join two nodes.
    CrossJoin(CrossJoin),
    /// A base table scan.
    Scan(Scan),
    /// Constant values.
    Values(Values),
}

#[derive(Debug)]
pub struct Filter {
    pub predicate: ScalarExpr,
    pub input: Box<RelationalPlan>,
}

#[derive(Debug)]
pub struct Project {
    /// A list of expressions to evaluate. The may introduce new values.
    pub expressions: Vec<ScalarExpr>,
    pub input: Box<RelationalPlan>,
}

#[derive(Debug)]
pub struct Scan {
    pub table: ResolvedTableReference,
    /// Schema describing the table with the projections applied.
    pub projected_schema: RelationSchema,
    /// An optional list of column indices to project.
    pub project: Option<Vec<usize>>,
    /// An optional list of filters to apply during scanning. Expressions should
    /// return booleans indicating if the row should be returned.
    pub filters: Option<Vec<ScalarExpr>>,
}

#[derive(Debug)]
pub struct Values {
    pub schema: RelationSchema,
    pub values: Vec<Vec<ScalarExpr>>,
}

#[derive(Debug)]
pub struct Join {
    pub left: Box<RelationalPlan>,
    pub right: Box<RelationalPlan>,
    pub join_type: JoinType,
    pub operator: JoinOperator,
}

#[derive(Debug, Clone)]
pub enum JoinType {
    Inner,
    Left,
    Right,
}

#[derive(Debug)]
pub enum JoinOperator {
    On(ScalarExpr),
}

#[derive(Debug)]
pub struct CrossJoin {
    pub left: Box<RelationalPlan>,
    pub right: Box<RelationalPlan>,
}

impl RelationalPlan {
    fn format(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        if depth > 0 {
            let leading = "| ".repeat(depth - 1);
            write!(f, "{}", leading)?;
        }
        let depth = depth + 1;

        match self {
            RelationalPlan::Project(project) => {
                writeln!(
                    f,
                    "Project: projections = {}",
                    DisplaySlice(&project.expressions)
                )?;
                project.input.format(f, depth)?;
            }
            RelationalPlan::Filter(filter) => {
                writeln!(f, "Filter: predicate = {}", filter.predicate)?;
                filter.input.format(f, depth)?;
            }
            RelationalPlan::Join(join) => {
                write!(
                    f,
                    "Join: type = {}, ",
                    match join.join_type {
                        JoinType::Inner => "inner",
                        JoinType::Left => "left",
                        JoinType::Right => "right",
                    },
                )?;
                match &join.operator {
                    JoinOperator::On(expr) => writeln!(f, "operator = on ({})", expr)?,
                };
                join.left.format(f, depth)?;
                join.right.format(f, depth)?;
            }
            RelationalPlan::CrossJoin(join) => {
                writeln!(f, "Cross join:")?;
                join.left.format(f, depth)?;
                join.right.format(f, depth)?;
            }
            RelationalPlan::Scan(scan) => {
                write!(f, "Scan: table = {}, ", scan.table,)?;
                match &scan.project {
                    Some(idxs) => write!(f, "projection = {}, ", DisplaySlice(&idxs))?,
                    None => write!(f, "projection = None, ")?,
                };
                match &scan.filters {
                    Some(filters) => writeln!(f, "filters = {}", DisplaySlice(&filters))?,
                    None => writeln!(f, "filters = None")?,
                };
            }
            RelationalPlan::Values(values) => {
                writeln!(f, "Values: values = [")?;
                for value in values.values.iter() {
                    writeln!(f, "{}", DisplaySlice(value))?;
                }
                writeln!(f, "]")?;
            }
        };

        Ok(())
    }
}

impl fmt::Display for RelationalPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f, 0)
    }
}
