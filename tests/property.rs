use lamina_sql::binder::Binder;
use lamina_sql::execution::{ExecutionConfig, ExecutionMode, execute};
use lamina_sql::optimizer::{OptimizerOptions, optimize};
use lamina_sql::parse_sql;
use lamina_sql::physical::{JoinPreference, create_physical_plan};
use lamina_sql::storage::{Catalog, Table};
use lamina_sql::types::{DataType, Field, Value};
use proptest::prelude::*;

proptest! {
    #[test]
    fn parser_accepts_generated_arithmetic(a in -1000i64..1000, b in 1i64..1000) {
        let sql = format!("SELECT x + {a} * {b} AS result FROM numbers WHERE x >= -10 LIMIT 7");
        prop_assert!(parse_sql(&sql).is_ok());
    }

    #[test]
    fn tuple_and_batch_execution_are_equivalent(values in prop::collection::vec(-1000i64..1000, 0..100)) {
        let mut catalog = Catalog::default();
        let rows = values.into_iter().map(|value| vec![Value::Int64(value)]).collect();
        catalog.register(Table::from_rows("numbers", vec![Field { qualifier: None, name: "x".into(), data_type: DataType::Int64, nullable: false }], rows).unwrap());
        let logical = Binder::new(&catalog).bind(&parse_sql("SELECT x * 3 AS y FROM numbers WHERE x >= 0 ORDER BY y").unwrap()).unwrap();
        let optimized = optimize(logical, OptimizerOptions::default()).unwrap().0;
        let physical = create_physical_plan(&optimized, JoinPreference::Auto);
        let vector = execute(&physical, &ExecutionConfig { mode: ExecutionMode::Vectorized, batch_size: 16 }).unwrap().data.rows();
        let tuple = execute(&physical, &ExecutionConfig { mode: ExecutionMode::Tuple, batch_size: 1 }).unwrap().data.rows();
        prop_assert_eq!(vector, tuple);
    }
}
