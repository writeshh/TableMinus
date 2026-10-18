//! Requests parsed and classed, then against a local Elasticsearch (`scripts/test-env.sh up`, security off).

use super::{Edit, Entry, Outcome, Session, apply, entry, open_session, reads, run, sort_of};
use hyper::Method;
use serde_json::{Value, json};

#[test]
fn entry_should_tell_a_request_from_sql() {
    assert_eq!(entry("select * from orders;").unwrap(), Entry::Sql("select * from orders".into()));
    assert_eq!(
        entry("get orders/_search\n{\"size\": 1}").unwrap(),
        Entry::Request { method: Method::GET, path: "/orders/_search".into(), body: Some(json!({ "size": 1 })) }
    );
    assert!(entry("GET /a b").is_err());
    assert!(entry("POST /a/_doc\n{not json").is_err());
}

#[test]
fn reads_should_take_searches_and_refuse_writes() {
    let r = |t: &str| reads(&entry(t).unwrap());
    assert!(r("SELECT 1") && r("GET /_cat/indices") && r("POST /orders/_search\n{}") && r("POST /_sql?format=txt\n{}"));
    assert!(
        !r("POST /orders/_doc\n{}")
            && !r("DELETE /orders")
            && !r("PUT /orders")
            && !r("POST /orders/_delete_by_query\n{}")
    );
}

#[test]
fn sort_of_should_read_field_and_order() {
    assert_eq!(sort_of("age:desc, name"), json!([{ "age": { "order": "desc" } }, { "name": { "order": "asc" } }]));
}

async fn local(index: &str) -> Session {
    let saved = serde_json::from_value(
        json!({ "name": "t", "kind": "elasticsearch", "url": "elasticsearch://127.0.0.1:9200" }),
    )
    .unwrap();
    let s = open_session(saved, None, None).await.unwrap();
    let _ = s.call(Method::DELETE, &format!("/{index}"), None).await; // left by an earlier run, or not
    s.call(
        Method::PUT,
        &format!("/{index}"),
        Some(&json!({ "mappings": { "properties": { "name": { "type": "keyword" }, "age": { "type": "integer" } } } })),
    )
    .await
    .unwrap();
    s
}

fn docs(o: &Outcome) -> &[Value] {
    match o {
        Outcome::Docs { docs, .. } => docs,
        Outcome::Value { value, .. } => panic!("not documents: {value}"),
    }
}

#[test]
#[ignore = "needs Elasticsearch on 127.0.0.1:9200 without security (scripts/test-env.sh up); replaces index tm_pets"]
fn local_elastic_should_edit_page_and_query_documents() {
    tauri::async_runtime::block_on(async {
        let s = local("tm_pets").await;
        assert!(s.server.starts_with("Elasticsearch 8."), "{}", s.server);
        let ins = |doc: &str| Edit::Insert { index: "tm_pets".into(), document: doc.into() };
        apply(&s, ins(r#"{"_id": "1", "name": "Rex", "age": 3}"#)).await.unwrap();
        apply(&s, ins(r#"{"_id": "2", "name": "Tom", "age": 5, "kind": "cat"}"#)).await.unwrap();
        assert!(
            apply(&s, ins(r#"{"_id": "1", "name": "Again"}"#)).await.is_err(),
            "an _id that exists isn't overwritten"
        );
        apply(&s, Edit::Set { index: "tm_pets".into(), id: "1".into(), field: "age".into(), value: "4".into() })
            .await
            .unwrap();
        apply(&s, Edit::Remove { index: "tm_pets".into(), id: "2".into(), field: "kind".into() }).await.unwrap();
        assert!(
            apply(
                &s,
                Edit::Set { index: "tm_pets".into(), id: "1".into(), field: "_id".into(), value: "\"x\"".into() }
            )
            .await
            .is_err()
        );

        assert!(s.indices().await.unwrap().contains(&"tm_pets".to_string()));
        let out = run(&s, entry("POST /tm_pets/_search\n{\"query\": {\"query_string\": {\"query\": \"age:>3\"}}, \"sort\": [{\"age\": \"asc\"}]}").unwrap(), 100).await.unwrap();
        assert_eq!(
            docs(&out[0]),
            [json!({ "_id": "1", "name": "Rex", "age": 4 }), json!({ "_id": "2", "name": "Tom", "age": 5 })]
        );
        let sql = run(&s, entry("SELECT name, age FROM tm_pets ORDER BY age DESC").unwrap(), 100).await.unwrap();
        assert_eq!(docs(&sql[0])[0], json!({ "name": "Tom", "age": 5 }));

        apply(&s, Edit::Replace { index: "tm_pets".into(), id: "2".into(), document: r#"{"name": "Max"}"#.into() })
            .await
            .unwrap();
        let e =
            apply(&s, Edit::Replace { index: "tm_pets".into(), id: "2".into(), document: r#"{"_id": "9"}"#.into() })
                .await
                .unwrap_err();
        assert!(e.to_string().contains("can't change"), "{e}");
        assert_eq!(
            apply(&s, Edit::Delete { index: "tm_pets".into(), ids: vec!["1".into(), "2".into()] }).await.unwrap(),
            "Deleted 2"
        );
        let gone = apply(&s, Edit::Replace { index: "tm_pets".into(), id: "2".into(), document: "{}".into() })
            .await
            .unwrap_err();
        assert!(gone.to_string().contains("gone"), "{gone}");
        let e = run(&s, entry("SELEC nonsense").unwrap(), 10).await.unwrap_err().to_string();
        assert!(e.starts_with("Elasticsearch: "), "{e}");
    });
}
