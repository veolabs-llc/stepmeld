//! stepmeld on AWS (docs/PLACEMENT.md §4): the kinds a deployment may
//! name once this crate is in the build ([`kinds`]).
//!
//! | Kind | Of | Settings |
//! |---|---|---|
//! | `s3` | store | `bucket`, `prefix` (default `stepmeld/`) |
//! | `aws-secretsmanager` | secret | `id`, `key` (one field of a JSON secret) |
//! | `aws` | sign-in | none: a registry login for an ECR image, and credentials for the container |
//! | `batch` | Performer (cloud) | `[programs.<name>.batch]`: see [`batch`]; the Runs' folders at `[providers.aws] runs` |
//!
//! Every kind also reads `[providers.aws]`: `region`, `profile`, and
//! `endpoint` (an S3-compatible endpoint, or a stand-in for tests); a
//! kind's own settings may name `region` and `profile` too.
//!
//! ```toml
//! store = "shared"
//!
//! [providers.aws]
//! region = "us-east-2"
//! profile = "deployment"          # an AWS profile every machine defines
//!
//! [stores.shared]
//! kind = "s3"
//! bucket = "example-stepmeld"
//!
//! [secrets.FONT_TOKEN]
//! kind = "aws-secretsmanager"
//! id = "example/font-token"
//! ```

pub mod batch;
pub mod s3;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use stepmeld_config::{Deployment, Kinds, PerformerKind, ProgramPlan, WorkerPlan};
use stepmeld_core::objects::ObjectStore;
use stepmeld_core::performer::Performer;
use stepmeld_core::store::SharedStore;
use stepmeld_core::Error;
use stepmeld_local::{Image, SignedIn};
use toml::{Table, Value};

/// stepmeld's kinds, and AWS's.
pub fn kinds(k: Kinds) -> Kinds {
    k.with_store("s3", s3_store)
        .with_secret("aws-secretsmanager", secretsmanager_secret)
        .with_sign_in("aws", sign_in)
        .with_performer("batch", PerformerKind { cloud: true, make: batch_performer })
}

/// An `s3://bucket/prefix/` URL's bucket and key prefix.
pub fn s3_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("s3://")?;
    let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    (!bucket.is_empty()).then(|| (bucket.to_string(), prefix.to_string()))
}

fn batch_performer(w: &WorkerPlan, programs: &[ProgramPlan]) -> Result<Arc<dyn Performer>, Error> {
    let d = w.deployment;
    let url = d
        .provider("aws")
        .and_then(|p| p.get("runs"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Invalid("Batch keeps each Run's folder in S3: say where with [providers.aws] runs = \"s3://bucket/prefix/\"".into()))?;
    let (bucket, prefix) = s3_url(url).ok_or_else(|| Error::Invalid(format!("[providers.aws] runs is an s3:// URL, not {url:?}")))?;
    let mut parsed = BTreeMap::new();
    for p in programs {
        let mut program: batch::BatchProgram = Value::Table(p.settings.clone())
            .try_into()
            .map_err(|e: toml::de::Error| Error::Invalid(format!("[programs.{}.batch]: {}", p.name, e.message())))?;
        // the program's `env` and none of its secrets: a submit's
        // variables are readable in the job's description
        program.env = p.vars.clone();
        if let Some(secret) = p.env.keys().find(|k| !p.vars.contains_key(*k)) {
            return Err(Error::Invalid(format!(
                "[programs.{}] gives {secret} as a secret, and a Batch job takes its secrets from its job definition: name it there instead",
                p.name
            )));
        }
        parsed.insert(p.name.clone(), program);
    }
    let settings = Settings::of(d, &Table::new());
    let jobs = Arc::new(batch::BatchJobs::new(aws_sdk_batch::Client::new(&settings.sdk())));
    let folders = Arc::new(s3::S3Objects::new(settings.s3(), &bucket));
    Ok(Arc::new(batch::BatchPerformer::new(&w.name, jobs, folders, url, &prefix, parsed)))
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("stepmeld-aws")
            .build()
            .expect("a runtime for the AWS SDK")
    })
}

/// Run one of the SDK's futures to its end: stepmeld's seams are
/// blocking.
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    runtime().block_on(f)
}

/// A failure to ask AWS, in words: the call, what it was about, the
/// SDK's own account, and how to sign in when that is what it lacks.
pub(crate) fn refused(what: &str, about: &str, e: impl std::error::Error) -> Error {
    let mut words = format!("AWS: could not {what} {about}: {}", aws_sdk_s3::error::DisplayErrorContext(&e));
    if words.contains("credentials") || words.contains("token") || words.contains("expired") {
        words.push_str(" (signed in? `aws sso login --profile <profile>`)");
    }
    Error::Refused(words)
}

/// Where and as whom: `[providers.aws]`, overridden by a kind's own
/// settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub region: Option<String>,
    pub profile: Option<String>,
    pub endpoint: Option<String>,
}

impl Settings {
    pub fn of(d: &Deployment, own: &Table) -> Settings {
        let pick = |k: &str| own.get(k).or_else(|| d.provider("aws").and_then(|p| p.get(k))).and_then(Value::as_str).map(str::to_string);
        Settings {
            region: pick("region"),
            profile: pick("profile"),
            endpoint: pick("endpoint"),
        }
    }

    /// The SDK's configuration for these settings, built afresh, so a
    /// sign-in renewed since is used.
    pub fn sdk(&self) -> aws_config::SdkConfig {
        let s = self.clone();
        block_on(async move {
            let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest()).http_client(http_client());
            if let Some(r) = s.region {
                loader = loader.region(aws_config::Region::new(r));
            }
            if let Some(p) = s.profile {
                loader = loader.profile_name(p);
            }
            if let Some(e) = s.endpoint {
                loader = loader.endpoint_url(e);
            }
            loader.load().await
        })
    }

    pub fn s3(&self) -> aws_sdk_s3::Client {
        let mut b = aws_sdk_s3::config::Builder::from(&self.sdk()).request_checksum_calculation(aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired);
        if self.endpoint.is_some() {
            b = b.force_path_style(true);
        }
        aws_sdk_s3::Client::from_conf(b.build())
    }
}

fn http_client() -> aws_sdk_s3::config::SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring))
        .build_https()
}

fn s3_store(d: &Deployment, settings: &Table) -> Result<Arc<dyn SharedStore>, Error> {
    let bucket = settings.get("bucket").and_then(Value::as_str).ok_or_else(|| Error::Invalid("an s3 store names its `bucket`".into()))?;
    let prefix = settings.get("prefix").and_then(Value::as_str).unwrap_or("stepmeld/");
    let client = Settings::of(d, settings).s3();
    Ok(Arc::new(ObjectStore::new(s3::S3Objects::new(client, bucket), prefix)))
}

/// A secret's string, or one field of it when it is JSON and `key` is set.
fn secretsmanager_secret(d: &Deployment, settings: &Table) -> Result<String, Error> {
    let id = settings.get("id").and_then(Value::as_str).ok_or_else(|| Error::Invalid("an aws-secretsmanager secret names its `id`".into()))?;
    let client = aws_sdk_secretsmanager::Client::new(&Settings::of(d, settings).sdk());
    let text = block_on(async { client.get_secret_value().secret_id(id).send().await })
        .map_err(|e| refused("read the secret", id, e))?
        .secret_string()
        .map(str::to_string)
        .ok_or_else(|| Error::Refused(format!("secret {id} holds no string")))?;
    match settings.get("key").and_then(Value::as_str) {
        None => Ok(text),
        Some(key) => {
            let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Refused(format!("secret {id} is not JSON, so it has no {key:?}: {e}")))?;
            match doc.get(key) {
                Some(serde_json::Value::String(s)) => Ok(s.clone()),
                Some(other) => Ok(other.to_string()),
                None => Err(Error::Refused(format!("secret {id} has no {key:?}"))),
            }
        }
    }
}

/// The registry of an image reference, when it is ECR's.
pub fn ecr_registry(reference: &str) -> Option<&str> {
    let host = reference.split('/').next()?;
    (host.contains(".dkr.ecr.") && host.contains(".amazonaws.com")).then_some(host)
}

/// Sign a container in: log Docker in to ECR when the image is there,
/// and hand the container the deployment's credentials, by name, with
/// its region. Fresh for each Run, so the credentials are current.
fn sign_in(d: &Deployment, image: &Image) -> Result<SignedIn, String> {
    let settings = Settings::of(d, &Table::new());
    let sdk = settings.sdk();
    if let Some(registry) = ecr_registry(&image.reference) {
        let ecr = aws_sdk_ecr::Client::new(&sdk);
        let token = block_on(async { ecr.get_authorization_token().send().await }).map_err(|e| refused("sign in to", registry, e).to_string())?;
        let encoded = token.authorization_data().first().and_then(|a| a.authorization_token()).ok_or("ECR gave no authorization token")?;
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|e| e.to_string())?;
        let password = String::from_utf8_lossy(&decoded).split_once(':').map(|(_, p)| p.to_string()).ok_or("ECR's token is not user:password")?;
        let mut login = std::process::Command::new("docker")
            .args(["login", "--username", "AWS", "--password-stdin", registry])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run docker login: {e}"))?;
        use std::io::Write;
        login.stdin.take().ok_or("docker login took no input")?.write_all(password.as_bytes()).map_err(|e| e.to_string())?;
        let out = login.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!("docker login to {registry} failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
    }
    let provider = sdk.credentials_provider().ok_or("no AWS credentials provider: name a profile in [providers.aws]")?;
    use aws_credential_types::provider::ProvideCredentials;
    let creds = block_on(async { provider.provide_credentials().await }).map_err(|e| format!("AWS credentials: {e} (signed in? `aws sso login --profile <profile>`)"))?;
    let mut env = BTreeMap::from([
        ("AWS_ACCESS_KEY_ID".to_string(), creds.access_key_id().to_string()),
        ("AWS_SECRET_ACCESS_KEY".to_string(), creds.secret_access_key().to_string()),
    ]);
    if let Some(t) = creds.session_token() {
        env.insert("AWS_SESSION_TOKEN".into(), t.to_string());
    }
    if let Some(at) = creds.expiry().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
        env.insert("AWS_CREDENTIAL_EXPIRATION".into(), stepmeld_core::iso(at.as_secs()));
    }
    if let Some(r) = sdk.region() {
        env.insert("AWS_REGION".into(), r.to_string());
        env.insert("AWS_DEFAULT_REGION".into(), r.to_string());
    }
    Ok(SignedIn { env, args: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ecr_image_names_its_registry() {
        assert_eq!(ecr_registry("123456789012.dkr.ecr.us-east-2.amazonaws.com/team/typeset:3"), Some("123456789012.dkr.ecr.us-east-2.amazonaws.com"));
        assert_eq!(ecr_registry("registry.example/typeset:3"), None);
        assert_eq!(ecr_registry("typeset:3"), None);
    }

    #[test]
    fn a_kind_s_settings_override_the_provider_s() {
        let d = Deployment::parse("[providers.aws]\nregion = \"us-east-2\"\nprofile = \"p\"\n", None).unwrap();
        let own: Table = "region = \"eu-west-1\"".parse().unwrap();
        assert_eq!(
            Settings::of(&d, &own),
            Settings {
                region: Some("eu-west-1".into()),
                profile: Some("p".into()),
                endpoint: None
            }
        );
    }
}
