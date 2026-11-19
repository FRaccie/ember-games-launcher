use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::io::Write;
use tower::ServiceExt;

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        z.start_file(*name, opt).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn meta_for(bundle: &[u8]) -> Meta {
    Meta {
        schema: 1,
        launcher: "0.1.0".into(),
        os: "windows".into(),
        games: vec![GameMeta { id: "usf4".into(), channel: Some("stable".into()), tag: Some("v1.6.1".into()) }],
        note: "it froze".into(),
        bundle_sha256: hex::encode(Sha256::digest(bundle)),
    }
}

fn good_bundle() -> Vec<u8> {
    zip_of(&[("manifest.json", b"{}"), ("usf4/sf4e.log", b"line\n")])
}

fn state(dir: &std::path::Path) -> Arc<AppState> {
    Arc::new(AppState::new(Config {
        dir: dir.to_path_buf(),
        key: vec![7; 32],
        public: "https://logs.example".into(),
        trust_proxy: true,
        ..Config::default()
    }))
}

fn form(meta: &str, bundle: &[u8], ip: &str) -> Request<Body> {
    let b = "XbOuNdArY";
    let mut body = Vec::new();
    write!(body, "--{b}\r\nContent-Disposition: form-data; name=\"meta\"\r\n\r\n{meta}\r\n").unwrap();
    write!(body, "--{b}\r\nContent-Disposition: form-data; name=\"bundle\"; filename=\"logs.zip\"\r\n\r\n").unwrap();
    body.extend_from_slice(bundle);
    write!(body, "\r\n--{b}--\r\n").unwrap();
    Request::post("/v1/logs")
        .header("content-type", format!("multipart/form-data; boundary={b}"))
        .header("x-forwarded-for", format!("10.0.0.1, {ip}"))
        .body(Body::from(body))
        .unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn a_good_bundle_is_kept_and_its_signed_link_downloads_it() {
    let t = tempfile::tempdir().unwrap();
    let st = state(t.path());
    let app = router(st.clone());
    let bundle = good_bundle();
    let meta = serde_json::to_string(&meta_for(&bundle)).unwrap();
    let (status, body) = send(&app, form(&meta, &bundle, "192.0.2.4")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let code = body["code"].as_str().unwrap().to_string();
    assert!(is_code(&code), "{code}");
    assert_eq!(std::fs::read(t.path().join(format!("{code}.zip"))).unwrap(), bundle);
    let record = std::fs::read_to_string(t.path().join(format!("{code}.json"))).unwrap();
    assert!(!record.contains("192.0.2.4"), "no address is stored");

    let ok =
        app.clone().oneshot(Request::get(format!("/v1/logs/{code}?k={}", st.sign(&code))).body(Body::empty()).unwrap());
    let ok = ok.await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(ok.into_body().collect().await.unwrap().to_bytes().to_vec(), bundle);
    let bad = app.oneshot(Request::get(format!("/v1/logs/{code}?k={}", "00".repeat(16))).body(Body::empty()).unwrap());
    assert_eq!(bad.await.unwrap().status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bad_bundles_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let app = router(state(t.path()));
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (zip_of(&[("manifest.json", b"{}"), ("../evil.log", b"x")]), "name"),
        (zip_of(&[("manifest.json", b"{}"), ("usf4/run.exe", b"x")]), "name"),
        (zip_of(&[("manifest.json", b"{}"), ("sfv/sf4e.log", b"x")]), "name"),
        (zip_of(&[("usf4/sf4e.log", b"x")]), "manifest"),
        (b"not a zip".to_vec(), "zip"),
    ];
    for (i, (bundle, reason)) in cases.iter().enumerate() {
        let meta = serde_json::to_string(&meta_for(bundle)).unwrap();
        let (status, body) = send(&app, form(&meta, bundle, &format!("192.0.2.{i}"))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some(*reason)));
    }
    let bundle = good_bundle();
    let mut wrong = meta_for(&bundle);
    wrong.bundle_sha256 = "ab".repeat(32);
    let (status, body) = send(&app, form(&serde_json::to_string(&wrong).unwrap(), &bundle, "192.0.2.50")).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("sha256")));
    let mut odd = serde_json::to_value(meta_for(&bundle)).unwrap();
    odd["extra"] = serde_json::json!(1);
    let (status, _) = send(&app, form(&odd.to_string(), &bundle, "192.0.2.51")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown meta fields are refused");
    assert!(stored(t.path()).is_empty());
}

#[test]
fn too_many_entries_or_too_much_unpacked_is_refused() {
    let names: Vec<String> = (0..MAX_ENTRIES).map(|i| format!("usf4/{i}.log")).collect();
    let mut entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
    entries.push(("manifest.json", b"{}"));
    let many = zip_of(&entries);
    assert_eq!(check_bundle(&many, &meta_for(&many)), Err("entries"));

    let big = vec![b'a'; (MAX_UNPACKED + 1) as usize];
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("manifest.json", opt).unwrap();
    z.start_file("usf4/sf4e.log", opt).unwrap();
    z.write_all(&big).unwrap();
    let bomb = z.finish().unwrap().into_inner();
    assert!(bomb.len() < 1 << 20, "compresses small");
    assert_eq!(check_bundle(&bomb, &meta_for(&bomb)), Err("unpacked"));
}

#[tokio::test]
async fn an_address_gets_five_uploads_an_hour() {
    let t = tempfile::tempdir().unwrap();
    let app = router(state(t.path()));
    let bundle = good_bundle();
    let meta = serde_json::to_string(&meta_for(&bundle)).unwrap();
    for _ in 0..5 {
        assert_eq!(send(&app, form(&meta, &bundle, "198.51.100.9")).await.0, StatusCode::CREATED);
    }
    assert_eq!(send(&app, form(&meta, &bundle, "198.51.100.9")).await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(send(&app, form(&meta, &bundle, "198.51.100.10")).await.0, StatusCode::CREATED);
}

#[test]
fn the_daily_limit_holds_across_hours() {
    let t = tempfile::tempdir().unwrap();
    let st = state(t.path());
    let ip: IpAddr = "203.0.113.1".parse().unwrap();
    let start = 1_000_000;
    let admitted = (0..30).filter(|i| st.admit(ip, start + i * 2_000)).count();
    assert_eq!(admitted, 20);
    assert!(st.admit(ip, start + 30 * 2_000 + 86_400), "a day later it is free again");
}

#[tokio::test]
async fn a_bundle_over_the_size_limit_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let st = Arc::new(AppState::new(Config {
        dir: t.path().into(),
        key: vec![1; 32],
        trust_proxy: true,
        max_bundle: 1_000,
        ..Config::default()
    }));
    let app = router(st);
    let bundle = zip_of(&[("manifest.json", b"{}"), ("usf4/a.log", &[b'z'; 4_000])]);
    let bundle = [bundle, vec![0; 70_000]].concat();
    let meta = serde_json::to_string(&meta_for(&bundle)).unwrap();
    assert_eq!(send(&app, form(&meta, &bundle, "192.0.2.77")).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_full_store_refuses_new_bundles() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("L-0000-0000.zip"), vec![0; 2_000]).unwrap();
    let st = Arc::new(AppState::new(Config {
        dir: t.path().into(),
        key: vec![1; 32],
        trust_proxy: true,
        budget: 2_100,
        ..Config::default()
    }));
    let app = router(st);
    let bundle = good_bundle();
    let meta = serde_json::to_string(&meta_for(&bundle)).unwrap();
    assert_eq!(send(&app, form(&meta, &bundle, "192.0.2.88")).await.0, StatusCode::INSUFFICIENT_STORAGE);
}

#[test]
fn the_sweep_removes_only_expired_bundles() {
    let t = tempfile::tempdir().unwrap();
    let st = state(t.path());
    for c in ["L-0000-0001", "L-0000-0002"] {
        std::fs::write(t.path().join(format!("{c}.zip")), b"z").unwrap();
        std::fs::write(t.path().join(format!("{c}.json")), b"{}").unwrap();
    }
    let now = now();
    assert_eq!(st.sweep(now), 0);
    assert_eq!(st.sweep(now + 91 * 86_400), 2);
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 0);
}

#[test]
fn the_note_cannot_break_out_of_its_code_block() {
    let mut m = meta_for(b"");
    m.note = "```@everyone".into();
    let text = message("L-AAAA-BBBB", &m, 2048, 3, "https://x/y");
    assert_eq!(text.matches("```").count(), 2);
    assert!(text.contains("usf4 v1.6.1 (stable)"));
    assert!(text.contains("2 files, 2 KB"));
}

#[test]
fn codes_and_signatures() {
    for _ in 0..100 {
        assert!(is_code(&new_code()));
    }
    assert!(!is_code("L-0000-000I"));
    assert!(!is_code("../../etc/x"));
    let t = tempfile::tempdir().unwrap();
    let st = state(t.path());
    let k = st.sign("L-AAAA-BBBB");
    assert!(st.signed("L-AAAA-BBBB", &k));
    assert!(!st.signed("L-AAAA-BBBC", &k));
    assert!(!st.signed("L-AAAA-BBBB", "zz"));
}

#[tokio::test]
async fn grafana_gets_the_numbers_and_the_newest_bundles() {
    let t = tempfile::tempdir().unwrap();
    let st = state(t.path());
    let app = router(st.clone());
    let bundle = good_bundle();
    let meta = serde_json::to_string(&meta_for(&bundle)).unwrap();
    assert_eq!(send(&app, form(&meta, &bundle, "192.0.2.30")).await.0, StatusCode::CREATED);
    let mut wrong = meta_for(&bundle);
    wrong.bundle_sha256 = "cd".repeat(32);
    send(&app, form(&serde_json::to_string(&wrong).unwrap(), &bundle, "192.0.2.31")).await;

    let internal = internal_router(st.clone());
    let r = internal.clone().oneshot(Request::get("/metrics").body(Body::empty()).unwrap()).await.unwrap();
    let text = String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    for line in [
        "ember_logs_bundles_total 1",
        "ember_logs_game_bundles_total{game=\"usf4\",os=\"windows\"} 1",
        "ember_logs_refused_total{reason=\"sha256\"} 1",
        "ember_logs_kept_bundles 1",
        "ember_logs_kept_game_bundles{game=\"usf4\"} 1",
    ] {
        assert!(text.lines().any(|l| l == line), "missing {line} in\n{text}");
    }

    let r = internal.oneshot(Request::get("/recent?limit=5").body(Body::empty()).unwrap()).await.unwrap();
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["games"], "usf4 v1.6.1");
    assert_eq!(rows[0]["note"], "it froze");
    let code = rows[0]["code"].as_str().unwrap();
    assert!(rows[0]["link"].as_str().unwrap().ends_with(&format!("{code}?k={}", st.sign(code))));
}
