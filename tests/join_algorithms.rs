use lamina_sql::optimizer::OptimizerOptions;
use lamina_sql::physical::JoinPreference;
use lamina_sql::storage::Table;
use lamina_sql::types::{DataType, Field, Value};
use lamina_sql::{Engine, QueryResult};

fn field(name: &str) -> Field {
    Field {
        qualifier: None,
        name: name.into(),
        data_type: DataType::Int64,
        nullable: false,
    }
}

fn engine(preference: JoinPreference) -> Engine {
    let mut engine = Engine::default();
    engine.config.join_preference = preference;
    engine.config.optimizer = OptimizerOptions {
        join_order: false,
        ..OptimizerOptions::default()
    };
    engine.register(
        Table::from_rows(
            "left_t",
            vec![field("id"), field("value")],
            vec![
                vec![Value::Int64(1), Value::Int64(10)],
                vec![Value::Int64(2), Value::Int64(20)],
                vec![Value::Int64(2), Value::Int64(21)],
            ],
        )
        .unwrap(),
    );
    engine.register(
        Table::from_rows(
            "right_t",
            vec![field("id"), field("value")],
            vec![
                vec![Value::Int64(2), Value::Int64(200)],
                vec![Value::Int64(3), Value::Int64(300)],
            ],
        )
        .unwrap(),
    );
    engine
}

fn rows(preference: JoinPreference) -> Vec<Vec<Value>> {
    let QueryResult::Data { execution, .. } = engine(preference).query(
        "SELECT l.value AS left_value, r.value AS right_value FROM left_t l INNER JOIN right_t r ON l.id = r.id ORDER BY left_value",
    ).unwrap() else { panic!() };
    execution.data.rows()
}

#[test]
fn hash_and_nested_loop_preserve_duplicates_and_agree() {
    let nested = rows(JoinPreference::NestedLoop);
    let hash = rows(JoinPreference::Hash);
    assert_eq!(hash, nested);
    assert_eq!(
        hash,
        vec![
            vec![Value::Int64(20), Value::Int64(200)],
            vec![Value::Int64(21), Value::Int64(200)],
        ]
    );
}

#[test]
fn ambiguous_join_column_fails_during_binding() {
    let error = match engine(JoinPreference::Auto)
        .query("SELECT id FROM left_t l INNER JOIN right_t r ON l.id = r.id")
    {
        Ok(_) => panic!("ambiguous column unexpectedly bound"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("ambiguous column"));
}
