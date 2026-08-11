use lamina_sql::storage::Table;
use lamina_sql::types::{DataType, Field, Value};
use lamina_sql::{Engine, QueryResult};
use rusqlite::Connection;

fn field(name: &str, data_type: DataType) -> Field {
    Field {
        qualifier: None,
        name: name.into(),
        data_type,
        nullable: false,
    }
}

#[test]
fn aggregate_filter_order_matches_sqlite_oracle() {
    let rows = vec![
        vec![Value::Utf8("EU".into()), Value::Int64(20)],
        vec![Value::Utf8("US".into()), Value::Int64(5)],
        vec![Value::Utf8("EU".into()), Value::Int64(30)],
        vec![Value::Utf8("US".into()), Value::Int64(40)],
    ];
    let mut engine = Engine::default();
    engine.register(
        Table::from_rows(
            "sales",
            vec![
                field("region", DataType::Utf8),
                field("amount", DataType::Int64),
            ],
            rows,
        )
        .unwrap(),
    );
    let sql = "SELECT region, SUM(amount) AS total FROM sales WHERE amount >= 10 GROUP BY region ORDER BY total DESC";
    let QueryResult::Data { execution, .. } = engine.query(sql).unwrap() else {
        panic!()
    };
    let actual = execution
        .data
        .rows()
        .into_iter()
        .map(|row| (row[0].to_string(), row[1].to_string()))
        .collect::<Vec<_>>();

    let oracle = Connection::open_in_memory().unwrap();
    oracle
        .execute("CREATE TABLE sales(region TEXT, amount INTEGER)", [])
        .unwrap();
    for (region, amount) in [("EU", 20), ("US", 5), ("EU", 30), ("US", 40)] {
        oracle
            .execute("INSERT INTO sales VALUES (?1, ?2)", (region, amount))
            .unwrap();
    }
    let mut statement = oracle.prepare(sql).unwrap();
    let expected = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?.to_string()))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}
