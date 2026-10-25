//! The four deployment shapes and the served site (spec 3.5 §7.17, §7.22, A-I20): each shape
//! writes what it needs into the project and prices the month before deployment; `serve`
//! starts the kernel with device pairing and the edge with the holding page, and the setup
//! code pairs exactly one device.

use std::net::SocketAddr;
use std::path::Path;

use adk_deploy::DeploymentManifest;
use adk_harness::website::SiteRef;
use adk_website::deploy::{
    AgencyFile, ProjectRef, ServeOptions, Shape, plan, serve, serve_agency, write_plan,
};
use adk_website::kinds::template;
use adk_website::onboarding::cost::{CostInputs, ModelPrice};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::team::SiteTeam;
use adk_website::themes::ThemeId;
use awp_types::SiteKind;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn options(name: &str, domain: &str, kind: SiteKind) -> ScaffoldOptions {
    ScaffoldOptions {
        name: name.into(),
        domain: domain.into(),
        kind,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    }
}

/// One HTTP/1.1 exchange: the status and the body.
async fn http(addr: SocketAddr, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body.to_string()).unwrap_or_default();
    (status, body)
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

#[tokio::test]
async fn every_shape_writes_what_it_needs_and_prices_the_month() {
    let dir = tempfile::tempdir().unwrap();
    scaffold(dir.path(), &options("Juniper Bakery", "juniper.example", SiteKind::Business))
        .await
        .unwrap();
    let site = SiteRef::new(uuid::Uuid::nil(), "juniper-example", "https://juniper.example");
    let team = SiteTeam::new(site, &template(SiteKind::Business));
    let project = ProjectRef {
        name: "Juniper Bakery".into(),
        domain: "juniper.example".into(),
        sites: vec!["juniper.example".into(), "mvule.example".into()],
    };
    let price = ModelPrice { input_per_mtok: 300, output_per_mtok: 1_500 };

    let local = plan(Shape::Local, &project, &team, &CostInputs::small(0, 1_200, price)).unwrap();
    assert!(local.files.is_empty());
    assert_eq!(local.commands, ["adk-rust up --install", "adk-website start ."]);

    let container =
        plan(Shape::Container, &project, &team, &CostInputs::small(500, 1_200, price)).unwrap();
    let dockerfile = &container.files["Dockerfile"];
    assert!(dockerfile.contains("--edge\", \"0.0.0.0:8080\""), "the edge listens publicly");
    assert!(dockerfile.contains("--kernel\", \"127.0.0.1:7340\""), "the kernel stays on loopback");
    assert!(dockerfile.contains("cargo install adk-website --version"), "from crates.io");
    assert!(container.files[".dockerignore"].starts_with("data\n"), "keys never enter the image");
    assert!(container.estimate.total > local.estimate.total, "hosting is priced");

    let managed =
        plan(Shape::Managed, &project, &team, &CostInputs::small(2_000, 1_200, price)).unwrap();
    let written = write_plan(&managed, dir.path()).await.unwrap();
    assert_eq!(written, ["adk-deploy.toml"]);
    let manifest = DeploymentManifest::from_path(&dir.path().join("adk-deploy.toml")).unwrap();
    assert_eq!(manifest.agent.binary, "adk-website");
    assert!(manifest.build.assets.contains(&"charter.draft.json".to_string()));
    assert!(write_plan(&managed, dir.path()).await.unwrap().is_empty(), "edits are kept");

    let agency = plan(Shape::Agency, &project, &team, &CostInputs::small(0, 0, price)).unwrap();
    let listed: AgencyFile = toml::from_str(&agency.files["agency.toml"]).unwrap();
    let domains: Vec<&str> = listed.sites.iter().map(|s| s.domain.as_str()).collect();
    assert_eq!(domains, ["juniper.example", "mvule.example"]);
    assert_eq!(listed.sites[1].dir, Path::new("sites/mvule.example"));
}

#[tokio::test]
async fn serve_starts_the_kernel_and_the_edge_and_the_setup_code_pairs_one_device() {
    let dir = tempfile::tempdir().unwrap();
    scaffold(dir.path(), &options("Juniper Bakery", "juniper.example", SiteKind::Business))
        .await
        .unwrap();
    let served = serve(ServeOptions {
        dir: dir.path().to_path_buf(),
        edge: loopback(),
        kernel: loopback(),
        edge_url: None,
        owner: "amina".into(),
    })
    .await
    .unwrap();
    assert_eq!(served.sites.len(), 1);

    // Before the launch the edge serves the holding page and the capabilities.
    let (status, page) = http(served.edge_addr, "GET", "/", None).await;
    assert_eq!(status, 200);
    assert!(page.contains("Juniper Bakery"), "{page}");
    let (status, manifest) = http(served.edge_addr, "GET", "/awp/v2/manifest", None).await;
    assert_eq!(status, 200, "{manifest}");
    assert!(manifest.contains("submit_inquiry"));
    let (status, _) = http(served.edge_addr, "GET", "/nothing-here", None).await;
    assert_eq!(status, 404);

    // A-I20: the code from the logs pairs one device; a second claim fails.
    let kernel = served.kernel_addr;
    let claim = json!({ "code": served.setup_code.code, "name": "Amina's phone" });
    let (status, paired) = http(kernel, "POST", "/api/kernel/pairing/claim", Some(&claim)).await;
    assert!((200..300).contains(&status), "{status} {paired}");
    assert!(paired.contains("token"), "{paired}");
    let (status, _) = http(kernel, "POST", "/api/kernel/pairing/claim", Some(&claim)).await;
    assert!(status >= 400, "a code pairs one device");

    // The keys were made on first start, owner-only.
    let seed = dir.path().join("data/keys/root.seed");
    assert_eq!(std::fs::read(&seed).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&seed).unwrap().permissions().mode() & 0o777, 0o600);
    }
    served.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agency_serves_each_site_at_its_own_host() {
    let dir = tempfile::tempdir().unwrap();
    for (name, domain, kind) in [
        ("Juniper Bakery", "juniper.example", SiteKind::Business),
        ("Mvule Clinic", "mvule.example", SiteKind::Health),
    ] {
        scaffold(&dir.path().join("sites").join(domain), &options(name, domain, kind))
            .await
            .unwrap();
    }
    let file = dir.path().join("agency.toml");
    std::fs::write(
        &file,
        "[[site]]\ndomain = \"juniper.example\"\ndir = \"sites/juniper.example\"\n\n\
         [[site]]\ndomain = \"mvule.example\"\ndir = \"sites/mvule.example\"\n",
    )
    .unwrap();
    let served = serve_agency(&file, loopback(), loopback(), "agency-owner").await.unwrap();
    assert_eq!(served.sites.len(), 2);
    let addr = served.edge_addr;
    let get = |host: &'static str| async move {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).into_owned()
    };
    assert!(get("juniper.example").await.contains("Juniper Bakery"));
    assert!(get("mvule.example").await.contains("Mvule Clinic"));
    assert!(get("other.example").await.starts_with("HTTP/1.1 404"));
    served.shutdown().await.unwrap();
}
