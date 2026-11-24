//! S1.36 contra los sitios de verdad (red): `cargo test -p iuredesk-engine --test news_live -- --ignored`.

use iuredesk_engine::news::{self, NewsPref};

#[tokio::test]
#[ignore]
async fn default_sources_list_news_and_their_audio_downloads() {
    let r = news::fetch_all(NewsPref::default().sources).await;
    let dir = tempfile::tempdir().unwrap();
    for s in &r.news {
        let items = s.result.as_ref().unwrap_or_else(|e| panic!("{}: {}", s.source.name, e.get()));
        for i in news::latest(items, 3) {
            eprintln!("{} · {} · audio: {}", s.source.name, i.title, i.has_audio());
        }
        assert!(!items.is_empty(), "{}: sin artículos", s.source.name);
        // Los artículos de Iurefficient (feed) no traen audio; los WordPress, su resumen.
        if s.source.audio_category.is_none() {
            continue;
        }
        let audio =
            items.iter().find(|i| i.has_audio()).unwrap_or_else(|| panic!("{}: sin artículo con audio", s.source.name));
        let path = news::fetch_audio(&audio.link, &audio.audio, dir.path()).await.unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        eprintln!("   audio {} ({} KB)", path.display(), len / 1024);
        assert!(len > 100_000);
        // La segunda vez sale de la caché.
        assert_eq!(news::fetch_audio(&audio.link, &audio.audio, dir.path()).await.unwrap(), path);
    }
}

/// Un resumen de SesoLibre cuyo MP3 está en Buzzsprout (redirige a `audio.buzzsprout.com`).
#[tokio::test]
#[ignore]
async fn sesolibre_summary_hosted_on_buzzsprout_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let link = "https://sesolibre.com/2026/11/22/resumen-de-noticias-domingo-22-de-noviembre-de-2026/";
    let path = news::fetch_audio(link, &news::AudioRef::Page, dir.path()).await.unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    eprintln!("buzzsprout {} ({} KB)", path.display(), len / 1024);
    assert!(len > 100_000);
}
