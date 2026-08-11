use crate::ast::Statement;
use crate::binder::Binder;
use crate::execution::{ExecutionConfig, ExecutionResult, execute};
use crate::optimizer::{OptimizationReport, OptimizerOptions, optimize};
use crate::physical::{JoinPreference, create_physical_plan};
use crate::storage::{Catalog, Table, import_csv};
use crate::{Result, parse_sql};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub optimizer: OptimizerOptions,
    pub execution: ExecutionConfig,
    pub join_preference: JoinPreference,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            optimizer: OptimizerOptions::default(),
            execution: ExecutionConfig::default(),
            join_preference: JoinPreference::Auto,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QueryTrace {
    pub ast_json: String,
    pub logical_plan: String,
    pub optimized_logical_plan: String,
    pub physical_plan: String,
    pub optimization: OptimizationReport,
}

pub enum QueryResult {
    Explain(String),
    Data {
        execution: Box<ExecutionResult>,
        trace: QueryTrace,
    },
}

#[derive(Default)]
pub struct Engine {
    catalog: Catalog,
    pub config: EngineConfig,
}

impl Engine {
    pub fn register(&mut self, table: Table) {
        self.catalog.register(table);
    }
    pub fn import_csv(&mut self, name: &str, path: impl AsRef<Path>) -> Result<()> {
        self.register(import_csv(path, name)?);
        Ok(())
    }
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        let statement = parse_sql(sql)?;
        let ast_json =
            serde_json::to_string_pretty(&statement).expect("AST serialization is infallible");
        let logical = Binder::new(&self.catalog).bind(&statement)?;
        let logical_plan = logical.format_tree();
        let (optimized, optimization) = optimize(logical, self.config.optimizer)?;
        let optimized_logical_plan = optimized.format_tree();
        let physical = create_physical_plan(&optimized, self.config.join_preference);
        let physical_plan = physical.format_tree();
        match statement {
            Statement::Explain { physical: true, .. } => Ok(QueryResult::Explain(physical_plan)),
            Statement::Explain {
                physical: false, ..
            } => Ok(QueryResult::Explain(optimized_logical_plan)),
            Statement::Select(_) => {
                let execution = execute(&physical, &self.config.execution)?;
                Ok(QueryResult::Data {
                    execution: Box::new(execution),
                    trace: QueryTrace {
                        ast_json,
                        logical_plan,
                        optimized_logical_plan,
                        physical_plan,
                        optimization,
                    },
                })
            }
        }
    }
}
