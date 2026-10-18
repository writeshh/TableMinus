//! Statements classed, then against a local Cassandra (`scripts/test-env.sh up`).

use super::{Edit, Session, apply, as_json, describe, open_session, page, reads};
use serde_json::{Map, Value, json};

#[test]
fn as_json_should_ask_selects_for_json_once() {
    assert_eq!(as_json("select * from t"), "SELECT JSON * from t");
    assert_eq!(as_json("SELECT JSON a FROM t"), "SELECT JSON a FROM t");
    assert_eq!(as_json("update t set a = 1"), "update t set a = 1");
    assert!(
        reads("SELECT 1") && reads("describe tables") && !reads("INSERT INTO t (a) VALUES (1)") && !reads("TRUNCATE t")
    );
}

async fn local(table: &str) -> Session {
    let saved =
        serde_json::from_value(json!({ "name": "t", "kind": "cassandra", "url": "cassandra://127.0.0.1:9042" }))
            .unwrap();
    let s = open_session(saved, None, None, None).await.unwrap();
    s.query(
        "CREATE KEYSPACE IF NOT EXISTS tm WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1}",
        &[],
    )
    .await
    .unwrap();
    s.query(&format!("DROP TABLE IF EXISTS tm.{table}"), &[]).await.unwrap();
    s.query(&format!("CREATE TABLE tm.{table} (owner text, id int, name text, tags set<text>, born timestamp, PRIMARY KEY (owner, id))"), &[]).await.unwrap();
    let saved =
        serde_json::from_value(json!({ "name": "t", "kind": "cassandra", "url": "cassandra://127.0.0.1:9042/tm" }))
            .unwrap();
    open_session(saved, None, None, None).await.unwrap()
}

fn key(owner: &str, id: i32) -> Map<String, Value> {
    serde_json::from_value(json!({ "owner": owner, "id": id })).unwrap()
}

#[test]
#[ignore = "needs Cassandra on 127.0.0.1:9042 (scripts/test-env.sh up); replaces keyspace tm's table pets"]
fn local_cassandra_should_page_rows_as_json_and_edit_by_key() {
    tauri::async_runtime::block_on(async {
        let s = local("pets").await;
        assert!(s.server.starts_with("Cassandra "), "{}", s.server);
        assert_eq!(s.tables().await.unwrap(), ["pets"]);
        assert_eq!(
            describe(&s, "pets").await.unwrap().keys,
            [("owner".to_string(), "text".to_string()), ("id".to_string(), "int".to_string())]
        );
        let ins = |row: &str| Edit::Insert { table: "pets".into(), row: row.into() };
        for i in 0..5 {
            apply(&s, ins(&format!(r#"{{"owner": "ada", "id": {i}, "name": "pet{i}", "tags": ["a"], "born": "2024-01-02 03:04:05.000Z"}}"#))).await.unwrap();
        }
        assert!(
            apply(&s, ins(r#"{"owner": "ada", "id": 1}"#)).await.unwrap_err().to_string().contains("exists already")
        );

        let first = page(&s, "SELECT JSON * FROM tm.pets", 2, None).await.unwrap();
        assert_eq!(first.rows.len(), 2);
        let rest = page(&s, "SELECT JSON * FROM tm.pets", 10, first.next.as_deref()).await.unwrap();
        assert_eq!(rest.rows.len(), 3, "the next page carries on");
        assert_eq!(first.rows[0]["tags"], json!(["a"]));

        apply(
            &s,
            Edit::Set { table: "pets".into(), key: key("ada", 1), field: "name".into(), value: "\"Rex\"".into() },
        )
        .await
        .unwrap();
        apply(&s, Edit::Remove { table: "pets".into(), key: key("ada", 1), field: "tags".into() }).await.unwrap();
        let one = s.query("SELECT JSON * FROM tm.pets WHERE owner = 'ada' AND id = 1", &[]).await.unwrap();
        assert_eq!((one[0]["name"].clone(), one[0]["tags"].clone()), (json!("Rex"), Value::Null));
        assert!(
            apply(&s, Edit::Set { table: "pets".into(), key: key("ada", 1), field: "id".into(), value: "9".into() })
                .await
                .is_err()
        );
        let gone = apply(
            &s,
            Edit::Set { table: "pets".into(), key: key("bob", 9), field: "name".into(), value: "\"x\"".into() },
        )
        .await
        .unwrap_err();
        assert!(gone.to_string().contains("gone"), "{gone}");

        apply(
            &s,
            Edit::Replace {
                table: "pets".into(),
                key: key("ada", 2),
                row: r#"{"owner": "ada", "id": 2, "name": "Max"}"#.into(),
            },
        )
        .await
        .unwrap();
        let two = s.query("SELECT JSON * FROM tm.pets WHERE owner = 'ada' AND id = 2", &[]).await.unwrap();
        assert_eq!((two[0]["name"].clone(), two[0]["born"].clone()), (json!("Max"), Value::Null), "left out is null");
        assert!(
            apply(
                &s,
                Edit::Replace { table: "pets".into(), key: key("ada", 2), row: r#"{"owner": "x", "id": 2}"#.into() }
            )
            .await
            .is_err()
        );
        assert_eq!(
            apply(&s, Edit::Delete { table: "pets".into(), keys: vec![key("ada", 0), key("ada", 2)] }).await.unwrap(),
            "Deleted 2"
        );
        assert_eq!(s.query("SELECT count(*) FROM tm.pets", &[]).await.unwrap()[0]["count"], json!(3));
    });
}
