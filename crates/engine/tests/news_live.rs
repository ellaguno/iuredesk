//! S1.36 contra los sitios de verdad (red): `cargo test -p iuredesk-engine --test news_live -- --ignored`.

use iuredesk_engine::news::{self, NewsPref};

#[tokio::test]
#[ignore]
async fn default_sources_list_news_and_their_audio_downloads() {
    let r = news::fetch_all(NewsPref::default().sources).await;
    let dir = tempfile::tempdir().unwrap();
    for s in &r.news {
        let items = s.result.as_ref().unwrap_or_else(|e| panic!("{}: {}", s.source.name, e.get()));
        let shown = news::shown(items, chrono::Utc::now());
        let audio = shown.audio.unwrap_or_else(|| panic!("{}: sin resumen con audio", s.source.name));
        eprintln!("{} · {} · {}", s.source.name, audio.title, audio.link);
        for o in &shown.others {
            eprintln!("   también: {}", o.title);
        }
        let path = news::fetch_audio(&audio.link, &audio.audio, dir.path()).await.unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        eprintln!("   audio {} ({} KB)", path.display(), len / 1024);
        assert!(len > 100_000);
        // La segunda vez sale de la caché.
        assert_eq!(news::fetch_audio(&audio.link, &audio.audio, dir.path()).await.unwrap(), path);
    }
}
