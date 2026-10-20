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
}
