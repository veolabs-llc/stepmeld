//! The S3 store against a stand-in S3: the SDK's real requests, the
//! store's conformance suites, and a conditional write that loses.

mod fake_s3;

use stepmeld_aws::s3::S3Objects;
use stepmeld_aws::Settings;
use stepmeld_core::objects::{Cond, ObjectStore, Objects};

fn client(endpoint: &str) -> aws_sdk_s3::Client {
    // the stand-in takes any credentials
    std::env::set_var("AWS_ACCESS_KEY_ID", "test");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
    Settings {
        region: Some("us-east-2".into()),
        profile: None,
        endpoint: Some(endpoint.into()),
    }
    .s3()
}

#[test]
fn s3_objects_hold_their_conditions() {
    let fake = fake_s3::FakeS3::start();
    let o = S3Objects::new(client(&fake.endpoint), "b");
    assert!(o.get("a/x.json").unwrap().is_none(), "absent is None");
    let v1 = o.put("a/x.json", b"1", Cond::IfNoneMatch).unwrap().expect("created");
    assert!(o.put("a/x.json", b"2", Cond::IfNoneMatch).unwrap().is_none(), "created once");
    let v2 = o.put("a/x.json", b"2", Cond::IfMatch(&v1)).unwrap().expect("over its version");
    assert!(o.put("a/x.json", b"3", Cond::IfMatch(&v1)).unwrap().is_none(), "not over a stale one");
    assert_eq!(o.get("a/x.json").unwrap().unwrap(), (b"2".to_vec(), v2));
    o.put("a/y~z+1.json", b"", Cond::None).unwrap();
    fake.state.lock().unwrap().page = 1;
    assert_eq!(o.list("a/").unwrap(), ["a/x.json", "a/y~z+1.json"], "every page, keys as written");
    o.delete("a/x.json").unwrap();
    assert!(o.get("a/x.json").unwrap().is_none());
}

#[test]
fn the_s3_store_conforms() {
    let fake = fake_s3::FakeS3::start();
    let store = ObjectStore::new(S3Objects::new(client(&fake.endpoint), "b"), "stepmeld/");
    stepmeld_core::store::conformance::run(&store);
    stepmeld_core::orders::conformance::run(&store);
    let fresh = ObjectStore::new(S3Objects::new(client(&fake.endpoint), "b"), "removing/");
    stepmeld_core::orders::conformance::removing(&fresh);
}

/// `stepmeld-run` as a Batch job runs it: told its folder by
/// `STEPMELD_RUN`, it brings the request down, runs the program, and
/// puts the log and the outcome up, all through the SDK.
#[test]
fn stepmeld_run_keeps_the_folder_in_s3_in_step() {
    let fake = fake_s3::FakeS3::start();
    let o = S3Objects::new(client(&fake.endpoint), "runs-bucket");
    o.put(
        "stepmeld/runs/r1/request.json",
        br#"{"workflow":"wf","step":"s","run":"wf/s/1","definition":{"name":"d","version":1},"inputs":{"a":2},"parameters":{},"outputs":[]}"#,
        Cond::None,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("verb.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ngrep -q '\"a\":2' \"$1/request.json\" && echo 'got a = 2'\necho '{\"state\":\"succeeded\",\"outputs\":{}}' > \"$1/outcome.json\"\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stepmeld-run"))
        .args(["sh", &script.display().to_string()])
        .env("STEPMELD_RUN", "s3://runs-bucket/stepmeld/runs/r1/")
        .env("STEPMELD_S3_ENDPOINT", &fake.endpoint)
        .env("STEPMELD_RUN_DIR", dir.path().join("local"))
        .env("AWS_REGION", "us-east-2")
        .env("AWS_ACCESS_KEY_ID", "test")
        .env("AWS_SECRET_ACCESS_KEY", "test")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let log = String::from_utf8(o.get("stepmeld/runs/r1/log.txt").unwrap().unwrap().0).unwrap();
    assert!(log.contains("got a = 2"), "{log}");
    assert!(o.get("stepmeld/runs/r1/outcome.json").unwrap().is_some());
}
