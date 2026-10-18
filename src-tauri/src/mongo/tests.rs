//! Against a local `MongoDB` (`scripts/test-env.sh up`): the operations a script records, run for real.

use super::{Edit, Outcome, Session, execute, open_session, reads, shell};
use secrecy::SecretString;
use serde_json::json;

async fn local(db: &str) -> Session {
    let saved = serde_json::from_value(
        json!({ "name": "t", "kind": "mongodb", "url": "mongodb://tm@127.0.0.1:27018/?authSource=admin" }),
    )
    .unwrap();
    let s = open_session(saved, Some(SecretString::from("testpass".to_string())), None, Some(db.into())).await.unwrap();
    s.client.database(db).drop().await.unwrap();
    s
}

/// Runs `script` as `mongo_run` does, without the app around it.
async fn run(s: &mut Session, script: &str) -> Vec<Outcome> {
    let mut out = vec![];
    for op in shell::operations(script, &s.db).unwrap() {
        out.push(execute(&s.client, &mut s.db, op, 1000).await.unwrap());
    }
    out
}

fn docs(o: &Outcome) -> &[serde_json::Value] {
    match o {
        Outcome::Docs { docs, .. } => docs,
        other => panic!("not documents: {other:?}"),
    }
}

#[test]
#[ignore = "needs a local MongoDB on 27018, user tm (scripts/test-env.sh up); replaces database tm_shell"]
fn local_mongo_should_run_a_script_of_writes_and_reads_in_order() {
    tauri::async_runtime::block_on(async {
        let mut s = local("tm_shell").await;
        let out = run(
            &mut s,
            "db.pets.insertMany([{ _id: 1, name: 'Rex', at: ISODate('2024-01-02T00:00:00Z') }, { _id: 2, name: 'Tom', n: NumberLong(5) }])\n\
             db.pets.updateOne({ _id: 2 }, { $set: { kind: 'cat' } })\n\
             db.pets.find({}, { name: 1, at: 1, kind: 1 }).sort({ _id: -1 })\n\
             db.pets.countDocuments({ name: /^r/i })\n\
             db.pets.aggregate([{ $group: { _id: null, n: { $sum: 1 } } }])\n\
             db.pets.distinct('name')",
        )
        .await;
        assert!(matches!(&out[0], Outcome::Done { text, .. } if text == "Inserted 2 documents"));
        assert!(matches!(&out[1], Outcome::Done { text, .. } if text == "Matched 1, modified 1"));
        assert_eq!(
            docs(&out[2]),
            [
                json!({ "_id": 2, "name": "Tom", "kind": "cat" }),
                json!({ "_id": 1, "name": "Rex", "at": { "$date": "2024-01-02T00:00:00Z" } })
            ]
        );
        assert!(matches!(&out[3], Outcome::Value { value, .. } if *value == json!(1)));
        assert_eq!(docs(&out[4]), [json!({ "_id": null, "n": 2 })]);
        assert!(matches!(&out[5], Outcome::Value { value, .. } if *value == json!(["Rex", "Tom"])));
    });
}

#[test]
#[ignore = "needs a local MongoDB on 27018, user tm (scripts/test-env.sh up); replaces database tm_pages"]
fn local_mongo_find_should_say_when_there_are_more_than_it_read() {
    tauri::async_runtime::block_on(async {
        let mut s = local("tm_pages").await;
        run(&mut s, "db.n.insertMany([{a:1},{a:2},{a:3}])").await;
        let ops = shell::operations("db.n.find().sort({ a: 1 })", &s.db).unwrap().into_iter().next().unwrap();
        let Outcome::Docs { docs, more, .. } = execute(&s.client, &mut s.db, ops, 2).await.unwrap() else { panic!() };
        assert_eq!((docs.len(), more), (2, true));
        let out = run(&mut s, "db.n.find().limit(3)").await;
        assert!(matches!(&out[0], Outcome::Docs { docs, more: false, .. } if docs.len() == 3));
        let out = run(&mut s, "db.n.find().skip(1).limit(1)").await;
        assert!(matches!(&out[0], Outcome::Docs { docs, more: false, .. } if docs.len() == 1));
    });
}

#[test]
#[ignore = "needs a local MongoDB on 27018, user tm (scripts/test-env.sh up); replaces database tm_admin"]
fn local_mongo_should_manage_indexes_collections_and_switch_databases() {
    tauri::async_runtime::block_on(async {
        let mut s = local("tm_admin").await;
        let out = run(
            &mut s,
            "db.createCollection('a')\n db.a.createIndex({ x: 1 }, { unique: true, name: 'by_x' })\n db.a.getIndexes()\n \
             db.a.renameCollection('b')\n show collections\n db.b.dropIndex('by_x')\n db.runCommand({ ping: 1 })\n use tm_admin2",
        )
        .await;
        assert!(matches!(&out[1], Outcome::Done { text, .. } if text == "Created index by_x"));
        assert!(docs(&out[2]).iter().any(|i| i["name"] == "by_x" && i["key"] == json!({ "x": 1 })));
        assert_eq!(docs(&out[4]), [json!({ "name": "b" })]);
        assert!(matches!(&out[6], Outcome::Value { value, .. } if value["ok"] == json!(1.0)));
        assert_eq!(s.db, "tm_admin2", "use lasts");
    });
}

#[test]
#[ignore = "needs a local MongoDB on 27018, user tm (scripts/test-env.sh up); replaces database tm_edit"]
fn local_mongo_edits_should_set_unset_replace_insert_and_delete_by_id() {
    tauri::async_runtime::block_on(async {
        let mut s = local("tm_edit").await;
        run(&mut s, "db.p.insertOne({ _id: ObjectId('64b7f0c2a1b2c3d4e5f60718'), name: 'Rex', age: 3 })").await;
        let id = r#"{"$oid": "64b7f0c2a1b2c3d4e5f60718"}"#.to_string();
        let c = || "p".to_string();
        super::apply(&s, Edit::Set { collection: c(), id: id.clone(), field: "age".into(), value: "4".into() })
            .await
            .unwrap();
        super::apply(&s, Edit::Unset { collection: c(), id: id.clone(), field: "name".into() }).await.unwrap();
        let out = run(&mut s, "db.p.findOne()").await;
        assert_eq!(docs(&out[0]), [json!({ "_id": { "$oid": "64b7f0c2a1b2c3d4e5f60718" }, "age": 4 })]);
        super::apply(
            &s,
            Edit::Replace { collection: c(), id: id.clone(), document: "{ name: 'Max', tags: ['a'] }".into() },
        )
        .await
        .unwrap();
        let e = super::apply(&s, Edit::Replace { collection: c(), id: id.clone(), document: "{ _id: 7 }".into() })
            .await
            .unwrap_err();
        assert!(e.to_string().contains("_id can't change"), "{e}");
        assert!(
            super::apply(&s, Edit::Set { collection: c(), id: id.clone(), field: "_id".into(), value: "1".into() })
                .await
                .is_err()
        );
        super::apply(&s, Edit::Insert { collection: c(), document: "{ _id: 2, name: 'Tom' }".into() }).await.unwrap();
        let out = run(&mut s, "db.p.find().sort({ name: 1 })").await;
        assert_eq!(docs(&out[0]).iter().map(|d| d["name"].clone()).collect::<Vec<_>>(), [json!("Max"), json!("Tom")]);
        let deleted = super::apply(&s, Edit::Delete { collection: c(), ids: vec![id, "2".into()] }).await.unwrap();
        assert_eq!(deleted, "Deleted 2");
        let e = super::apply(&s, Edit::Set { collection: c(), id: "2".into(), field: "a".into(), value: "1".into() })
            .await
            .unwrap_err();
        assert!(e.to_string().contains("gone"), "{e}");
    });
}

#[test]
fn reads_should_tell_reading_operations_from_writing_ones() {
    let op = |script: &str| shell::operations(script, "test").unwrap().into_iter().next().unwrap();
    assert!(reads(&op("db.t.find({ a: 1 })")));
    assert!(reads(&op("db.t.aggregate([{ $match: {} }])")));
    assert!(!reads(&op("db.t.aggregate([{ $match: {} }, { $out: 'copy' }])")));
    assert!(reads(&op("db.runCommand({ ping: 1 })")));
    assert!(!reads(&op("db.runCommand({ drop: 't' })")));
    assert!(!reads(&op("db.t.deleteMany({})")));
}
