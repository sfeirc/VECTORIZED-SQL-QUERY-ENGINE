use lamina_sql::parse_sql;

fn main() {
    let sql = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if sql.is_empty() {
        eprintln!("usage: lamina <SQL>");
        std::process::exit(2);
    }
    match parse_sql(&sql) {
        Ok(statement) => println!("{statement:#?}"),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}
