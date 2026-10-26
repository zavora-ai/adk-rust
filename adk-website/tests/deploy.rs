//! The four deployment shapes (spec 3.5 §7.22, A-I22): each shape writes what it needs into
//! the project and prices the month before deployment; an agency serves each site at its own
//! host.

use std::net::SocketAddr;
use std::path::Path;

use adk_deploy::DeploymentManifest;
use adk_harness::website::SiteRef;
use adk_website::deploy::{
    AgencyFile, ProjectRef, ServeOptions, Shape, plan, serve_agency, write_plan,
};
use adk_website::kinds::template;
use adk_website::onboarding::cost::{CostInputs, ModelPrice};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::team::SiteTeam;
use adk_website::themes::ThemeId;
use awp_types::SiteKind;
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
    let mut options = ServeOptions::new(dir.path(), "agency-owner");
    options.edge = loopback();
    options.kernel = loopback();
    let served = serve_agency(&file, options).await.unwrap();
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
