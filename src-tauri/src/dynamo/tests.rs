//! Against `DynamoDB` Local (`scripts/test-env.sh up`): tables, pages, `PartiQL` and every kind of edit.

use super::{Edit, Session, apply, described, open_session, statement};
use secrecy::SecretString;
use serde_json::{Map, Value, json};

async fn local(table: &str) -> Session {
    let saved = serde_json::from_value(
        json!({ "name": "t", "kind": "dynamodb", "url": "dynamodb://tm@127.0.0.1:8001?region=eu-north-1" }),
    )
    .unwrap();
    let s = open_session(saved, Some(SecretString::from("secret".to_string())), None).await.unwrap();
    let _ = s.call("DeleteTable", &json!({ "TableName": table })).await; // left by an earlier run, or not
    s.call("CreateTable", &json!({
        "TableName": table,
        "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }, { "AttributeName": "n", "AttributeType": "N" }],
        "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }, { "AttributeName": "n", "KeyType": "RANGE" }],
        "BillingMode": "PAY_PER_REQUEST",
    }))
    .await
    .unwrap();
    s
}

fn key(pk: &str, n: u32) -> Map<String, Value> {
    serde_json::from_value(json!({ "pk": { "S": pk }, "n": { "N": n.to_string() } })).unwrap()
}

#[test]
#[ignore = "needs DynamoDB Local on 127.0.0.1:8001 (scripts/test-env.sh up); replaces table tm_items"]
fn local_dynamo_should_list_describe_page_and_run_partiql() {
    tauri::async_runtime::block_on(async {
        let s = local("tm_items").await;
        assert!(s.tables().await.unwrap().contains(&"tm_items".to_string()));
        let d = described(&s.call("DescribeTable", &json!({ "TableName": "tm_items" })).await.unwrap());
        assert_eq!(d.keys, [("pk".to_string(), "S".to_string()), ("n".to_string(), "N".to_string())]);
        for i in 0..5 {
            statement(
                &s,
                &format!("INSERT INTO \"tm_items\" VALUE {{'pk': 'a', 'n': {i}, 'tag': 'x{i}'}}"),
                None,
                None,
            )
            .await
            .unwrap();
        }
        let first = statement(&s, "SELECT * FROM \"tm_items\"", Some(2), None).await.unwrap();
        assert_eq!(first.items.len(), 2);
        let rest = statement(&s, "SELECT * FROM \"tm_items\"", Some(10), first.next.clone()).await.unwrap();
        assert_eq!(rest.items.len(), 3, "the next page carries on where the first stopped");
        let found =
            statement(&s, "SELECT * FROM \"tm_items\" WHERE \"pk\" = 'a' AND \"n\" = 3", None, None).await.unwrap();
        assert_eq!(found.items[0]["tag"], json!({ "S": "x3" }));
        let e = statement(&s, "SELEC 1", None, None).await.unwrap_err().to_string();
        assert!(e.contains("ValidationException"), "{e}");
    });
}

#[test]
#[ignore = "needs DynamoDB Local on 127.0.0.1:8001 (scripts/test-env.sh up); replaces table tm_edits"]
fn local_dynamo_edits_should_go_by_the_key_and_refuse_to_change_it() {
    tauri::async_runtime::block_on(async {
        let s = local("tm_edits").await;
        let insert = || Edit::Insert {
            table: "tm_edits".into(),
            key_names: vec!["pk".into(), "n".into()],
            item: "{'pk': 'a', 'n': 1, 'name': 'Rex'}".into(),
        };
        apply(&s, insert()).await.unwrap();
        assert!(apply(&s, insert()).await.unwrap_err().to_string().contains("exists already"));
        apply(&s, Edit::Set { table: "tm_edits".into(), key: key("a", 1), field: "age".into(), value: "4".into() })
            .await
            .unwrap();
        apply(&s, Edit::Remove { table: "tm_edits".into(), key: key("a", 1), field: "name".into() }).await.unwrap();
        let item = &statement(&s, "SELECT * FROM \"tm_edits\"", None, None).await.unwrap().items[0];
        assert_eq!(item["age"], json!({ "N": "4" }));
        assert!(item.get("name").is_none());
        assert!(
            apply(
                &s,
                Edit::Set { table: "tm_edits".into(), key: key("a", 1), field: "pk".into(), value: "'b'".into() }
            )
            .await
            .is_err()
        );
        apply(
            &s,
            Edit::Replace {
                table: "tm_edits".into(),
                key: key("a", 1),
                item: "{'pk': 'a', 'n': 1, 'tags': <<'x', 'y'>>}".into(),
            },
        )
        .await
        .unwrap();
        let e =
            apply(&s, Edit::Replace { table: "tm_edits".into(), key: key("a", 1), item: "{'pk': 'b', 'n': 1}".into() })
                .await
                .unwrap_err();
        assert!(e.to_string().contains("can't change"), "{e}");
        let gone =
            apply(&s, Edit::Set { table: "tm_edits".into(), key: key("zz", 9), field: "x".into(), value: "1".into() })
                .await
                .unwrap_err();
        assert!(gone.to_string().starts_with("DynamoDB: ConditionalCheckFailed"), "{gone}");
        assert_eq!(
            apply(&s, Edit::Delete { table: "tm_edits".into(), keys: vec![key("a", 1)] }).await.unwrap(),
            "Deleted 1"
        );
        assert_eq!(statement(&s, "SELECT * FROM \"tm_edits\"", None, None).await.unwrap().items, Vec::<Value>::new());
    });
}

#[test]
fn reads_should_take_only_selects() {
    assert!(super::reads("  select * from \"t\""));
    assert!(!super::reads("UPDATE \"t\" SET a = 1 WHERE pk = 'x'"));
    assert!(!super::reads("DELETE FROM \"t\" WHERE pk = 'x'"));
}
