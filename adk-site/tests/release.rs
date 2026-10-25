//! Releases (spec 3.5 §7.7, §7.8.3, A-I1; AWP 2.0 §14.1–§14.4): server code refuses the whole
//! release, identical inputs give identical manifests, deltas resolve to the full file set
//! and snapshots come on cadence.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use adk_core::ManualClock;
use adk_site::SiteError;
use adk_site::content::{SiteConfig, SiteProject};
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::release::{
    PreviousRelease, ReleaseBuilder, ReleaseOptions, full_file_set, manifest_digest,
};
use adk_site::scan::REFUSED_EXTENSIONS;
use awp_types::{ContentItem, ContentKind, ContentState, Markdown, Redirect, ReleaseSource};
use chrono::{TimeZone, Utc};
use proptest::prelude::*;

fn clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap()))
}

fn options() -> ReleaseOptions {
    ReleaseOptions {
        source: ReleaseSource::Onboarding,
        generator: BuiltinGenerator::reference(),
        charter_version: 1,
        redirects: Vec::new(),
        written: None,
        force_snapshot: false,
    }
}

fn write_site(dir: &Path, extra: Option<(&str, &[u8])>) {
    std::fs::create_dir_all(dir.join("about")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><html lang=\"en\"><title>x</title><p>home</p></html>",
    )
    .unwrap();
    std::fs::write(dir.join("about/index.html"), "<p>about</p>").unwrap();
    std::fs::write(dir.join("about/index.md"), "# About\n\n```php\n<?php echo 1; ?>\n```\n")
        .unwrap();
    if let Some((path, bytes)) = extra {
        let target = dir.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
}

#[tokio::test]
async fn every_refused_type_and_directive_fails_the_release() {
    let clean = tempfile::tempdir().unwrap();
    write_site(clean.path(), None);
    let builder = ReleaseBuilder::new().with_clock(clock());
    let staged = builder.stage(clean.path(), None, options()).await.unwrap();
    assert_eq!(
        staged.manifest.files.len(),
        3,
        "a clean site passes, a Markdown twin quoting PHP included"
    );

    let mut cases: Vec<(String, Vec<u8>)> = REFUSED_EXTENSIONS
        .iter()
        .map(|ext| (format!("uploads/file.{ext}"), b"x".to_vec()))
        .collect();
    cases.extend([
        ("uploads/shell.php.jpg".to_string(), b"\xff\xd8\xff".to_vec()),
        (".htaccess".to_string(), b"AddHandler cgi-script .jpg".to_vec()),
        ("blog/.user.ini".to_string(), b"auto_prepend_file=x".to_vec()),
        ("web.config".to_string(), b"<configuration/>".to_vec()),
        ("tools/run".to_string(), b"#!/bin/sh\nid\n".to_vec()),
        ("news/index.html".to_string(), b"<p><!--#include virtual=\"/x\" --></p>".to_vec()),
        ("news/exec.html".to_string(), b"<p><!--#exec cmd=\"id\" --></p>".to_vec()),
        ("news/php.html".to_string(), b"<p><?php echo 1; ?></p>".to_vec()),
        ("news/asp.htm".to_string(), b"<%@ Page Language=\"C#\" %>".to_vec()),
        ("images/logo.png".to_string(), b"\x7fELF\x02\x01\x01\x00".to_vec()),
        ("images/setup.png".to_string(), b"MZ\x90\x00\x03\x00".to_vec()),
        (
            "images/photo.jpg".to_string(),
            [b"\xff\xd8\xff\xe0\x80\x81".as_slice(), b"<?php system($_GET[0]); ?>"].concat(),
        ),
    ]);
    for (path, bytes) in cases {
        let dir = tempfile::tempdir().unwrap();
        write_site(dir.path(), Some((&path, &bytes)));
        let mut forced = options();
        forced.force_snapshot = true;
        match builder.stage(dir.path(), None, forced).await {
            Err(SiteError::ServerCode { findings }) => assert_eq!(findings[0].path, path),
            other => panic!("{path} was not refused: {other:?}"),
        }
    }
}

#[tokio::test]
async fn deltas_resolve_and_snapshots_come_on_cadence() {
    let builder = ReleaseBuilder::new().with_clock(clock()).snapshot_every(3);
    let dir = tempfile::tempdir().unwrap();
    write_site(dir.path(), None);
    let mut previous: Option<PreviousRelease> = None;
    let mut chain: Vec<awp_types::ReleaseManifest> = Vec::new();
    let mut kinds = Vec::new();
    for n in 1..=5u32 {
        std::fs::write(dir.path().join("index.html"), format!("<p>v{n}</p>")).unwrap();
        if n == 3 {
            std::fs::remove_file(dir.path().join("about/index.md")).unwrap();
        }
        let staged = builder.stage(dir.path(), previous.as_ref(), options()).await.unwrap();
        kinds.push(staged.manifest.base);
        // Resolve the full file set from manifests alone, as a verifier does.
        let mut resolved = BTreeMap::new();
        let mut manifests: Vec<&awp_types::ReleaseManifest> = Vec::new();
        let mut cursor = Some(&staged.manifest);
        while let Some(m) = cursor {
            manifests.push(m);
            cursor = m.base.map(|b| &chain[(b - 1) as usize]);
        }
        for m in manifests.iter().rev() {
            resolved = full_file_set(m, &resolved);
        }
        assert_eq!(resolved, staged.files, "release {n} resolves to its full file set");
        if let Some(prev) = &previous {
            assert_eq!(staged.manifest.previous.as_ref(), Some(&prev.digest));
        }
        chain.push(staged.manifest.clone());
        previous = Some(PreviousRelease::from_staged(&staged));
    }
    assert_eq!(kinds, vec![None, Some(1), Some(2), None, Some(4)]);
    assert!(chain[2].removed.contains("/about/index.md"));
    assert_eq!(
        chain[1].files.keys().collect::<Vec<_>>(),
        vec!["/index.html"],
        "a delta lists only changes"
    );
}

#[tokio::test]
async fn manifests_follow_the_wire_format() {
    let dir = tempfile::tempdir().unwrap();
    write_site(dir.path(), None);
    let mut opts = options();
    opts.redirects = vec![Redirect { from: "/old/".into(), to: "/".into(), status: 301 }];
    let staged =
        ReleaseBuilder::new().with_clock(clock()).stage(dir.path(), None, opts).await.unwrap();
    assert_eq!(staged.manifest.files["/index.html"].content_type, "text/html; charset=utf-8");
    assert_eq!(
        staged.manifest.files["/about/index.md"].content_type,
        "text/markdown; charset=utf-8"
    );
    let (digest, bytes) = manifest_digest(&staged.manifest).unwrap();
    assert_eq!(digest, staged.digest);
    assert_eq!(bytes, staged.manifest_bytes);
    assert!(bytes.starts_with(b"{\"charterVersion\":1,\"createdAt\":\"2026-10-25T09:00:00Z\",\"files\":{\"/about/index.html\""));

    let mut bad = options();
    bad.redirects = vec![Redirect { from: "/x/".into(), to: "/".into(), status: 200 }];
    assert!(ReleaseBuilder::new().stage(dir.path(), None, bad).await.is_err());
    std::fs::create_dir_all(dir.path().join(".well-known/awp/log")).unwrap();
    std::fs::write(dir.path().join(".well-known/awp/log/checkpoint"), "x").unwrap();
    assert!(matches!(
        ReleaseBuilder::new().stage(dir.path(), None, options()).await,
        Err(SiteError::InvalidContent { .. })
    ));
}

fn item(index: usize, kind: ContentKind, language: &str, title: &str, body: &str) -> ContentItem {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_string(), serde_json::Value::String(title.to_string()));
    ContentItem {
        id: format!("item-{index}-{language}").as_str().into(),
        kind,
        slug: format!("item-{index}"),
        language: language.parse().unwrap(),
        fields,
        volatile: Vec::new(),
        body: Some(Markdown(body.to_string())),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Published,
    }
}

fn site_strategy() -> impl Strategy<Value = Vec<ContentItem>> {
    let kinds = prop::sample::select(vec![
        ContentKind::Page,
        ContentKind::Post,
        ContentKind::Article,
        ContentKind::Product,
        ContentKind::Service,
        ContentKind::Person,
    ]);
    prop::collection::vec(
        (
            kinds,
            prop::sample::select(vec!["en", "fr"]),
            "[A-Za-z ]{1,20}",
            "[A-Za-z0-9 #*\\n<>&]{0,80}",
        ),
        0..8,
    )
    .prop_map(|specs| {
        specs
            .into_iter()
            .enumerate()
            .map(|(i, (kind, language, title, body))| item(i, kind, language, &title, &body))
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Two builds of the same input produce the same manifest digest.
    #[test]
    fn identical_inputs_give_identical_manifests(items in site_strategy()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let config = SiteConfig::new("Generated", "generated.example", vec!["en".parse().unwrap(), "fr".parse().unwrap()]);
            let project = SiteProject::new(config, items, Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap());
            let mut digests = Vec::new();
            for _ in 0..2 {
                let out = tempfile::tempdir().unwrap();
                let report = BuiltinGenerator.build(&project, out.path()).await.unwrap();
                let mut opts = options();
                opts.redirects = report.redirects.clone();
                let staged = ReleaseBuilder::new().with_clock(clock()).stage(out.path(), None, opts).await.unwrap();
                digests.push(staged.digest);
            }
            prop_assert_eq!(&digests[0], &digests[1]);
            Ok(())
        })?;
    }
}
