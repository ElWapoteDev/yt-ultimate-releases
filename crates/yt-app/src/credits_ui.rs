//! Credits dialog: who wrote, produced and released a song.
//! See `docs/systems/Browse.md`.

use crate::{
    app::{artwork_src, Thumb},
    icons::Icon,
    state::AppState,
};
use dioxus::prelude::*;
use yt_core::{fold, Credits, Track};

/// Spanish names of the roles that sources give in English: library tags,
/// YouTube's credits page and every role its auto-generated descriptions
/// gave in a sample of 88 art tracks on 2026-11-11. Roles match by
/// [`role_key`], and several may share a name.
const ROLE_LABELS: &[(&str, &str)] = &[
    ("composer lyricist", "Composición y letra"),
    ("composer", "Composición"),
    ("lyricist", "Letra"),
    ("writer", "Autoría"),
    ("songwriter", "Autoría"),
    ("author", "Autoría"),
    ("translator", "Traducción"),
    ("arranger", "Arreglos"),
    ("recording arranger", "Arreglos de grabación"),
    ("performance arranger", "Arreglos de interpretación"),
    ("work arranger", "Arreglos de la obra"),
    ("sub arranger", "Arreglos adicionales"),
    ("strings arranger", "Arreglos de cuerdas"),
    ("choir arranger", "Arreglos de coro"),
    ("orchestral arranger", "Arreglos orquestales"),
    ("orchestration", "Orquestación"),
    ("producer", "Producción"),
    ("executive producer", "Producción ejecutiva"),
    ("co producer", "Coproducción"),
    ("additional producer", "Producción adicional"),
    ("additional production", "Producción adicional"),
    ("vocal producer", "Producción vocal"),
    ("programmer", "Programación"),
    ("programming", "Programación"),
    ("programming engineer", "Programación"),
    ("drum programmer", "Programación de batería"),
    ("a&r", "A&R"),
    ("a&r director", "Dirección de A&R"),
    ("a&r direction", "Dirección de A&R"),
    ("a&r manager", "Gestión de A&R"),
    ("director", "Dirección"),
    ("conductor", "Dirección"),
    ("orchestral conductor", "Dirección de orquesta"),
    ("coordinator", "Coordinación"),
    ("consultant", "Asesoría"),
    ("recording supervisor", "Supervisión de grabación"),
    ("studio personnel", "Personal de estudio"),
    ("studio musician", "Músico de estudio"),
    ("engineer", "Ingeniería"),
    ("additional engineer", "Ingeniería adicional"),
    ("assistant engineer", "Asistencia de ingeniería"),
    ("recording engineer", "Grabación"),
    ("recording second engineer", "Asistencia de grabación"),
    ("assistant recording engineer", "Asistencia de grabación"),
    (
        "additional vocal recording engineer",
        "Grabación adicional de voz",
    ),
    ("vocal engineer", "Ingeniería de voz"),
    ("balance engineer", "Ingeniería de balance"),
    ("mixing engineer", "Mezcla"),
    ("mix engineer", "Mezcla"),
    ("mixer", "Mezcla"),
    ("co mixing engineer", "Mezcla en colaboración"),
    ("surround mixing engineer", "Mezcla envolvente"),
    ("mixing second engineer", "Asistencia de mezcla"),
    ("assistant mixing engineer", "Asistencia de mezcla"),
    ("assistant mixer", "Asistencia de mezcla"),
    ("mastering engineer", "Masterización"),
    ("masterer", "Masterización"),
    ("immersive mastering engineer", "Masterización inmersiva"),
    (
        "assistant mastering engineer",
        "Asistencia de masterización",
    ),
    ("digital editing engineer", "Edición digital"),
    ("vocal editing", "Edición de voz"),
    ("sound editor", "Edición de sonido"),
    ("remixer", "Remix"),
    ("performer", "Interpretación"),
    ("performance", "Interpretación"),
    ("associated performer", "Intérprete"),
    ("main artist", "Artista principal"),
    ("featured artist", "Artista invitado"),
    ("soloist", "Solista"),
    ("orchestra", "Orquesta"),
    ("all instruments", "Todos los instrumentos"),
    ("vocals", "Voz"),
    ("vocal", "Voz"),
    ("vocalist", "Voz"),
    ("lead vocals", "Voz principal"),
    ("lead vocalist", "Voz principal"),
    ("featured vocals", "Voz invitada"),
    ("additional vocalist", "Voces adicionales"),
    ("background vocal", "Coros"),
    ("background vocals", "Coros"),
    ("background vocalist", "Coros"),
    ("backing vocals", "Coros"),
    ("choir vocals", "Coro"),
    ("bass vocalist", "Voz bajo"),
    ("talkbox", "Talkbox"),
    ("guitar", "Guitarra"),
    ("electric guitar", "Guitarra eléctrica"),
    ("acoustic guitar", "Guitarra acústica"),
    ("rhythm guitar", "Guitarra rítmica"),
    ("requinto guitar", "Requinto"),
    ("vihuela", "Vihuela"),
    ("bajo quinto", "Bajo quinto"),
    ("bajo", "Bajo"),
    ("bass", "Bajo"),
    ("bass guitar", "Bajo"),
    ("acoustic bass guitar", "Bajo acústico"),
    ("double bass", "Contrabajo"),
    ("banjo", "Banjo"),
    ("drums", "Batería"),
    ("percussion", "Percusión"),
    ("bongos", "Bongós"),
    ("glockenspiel", "Glockenspiel"),
    ("keyboards", "Teclados"),
    ("piano", "Piano"),
    ("synthesizer", "Sintetizador"),
    ("organ", "Órgano"),
    ("hammond organ", "Órgano Hammond"),
    ("accordion", "Acordeón"),
    ("strings", "Cuerdas"),
    ("violin", "Violín"),
    ("viola", "Viola"),
    ("cello", "Violonchelo"),
    ("harp", "Arpa"),
    ("trumpet", "Trompeta"),
    ("french horn", "Corno francés"),
    ("trombone", "Trombón"),
    ("tenor trombone", "Trombón tenor"),
    ("bass trombone", "Trombón bajo"),
    ("tuba", "Tuba"),
    ("saxophone", "Saxofón"),
    ("alto saxophone", "Saxofón alto"),
    ("tenor saxophone", "Saxofón tenor"),
    ("baritone saxophone", "Saxofón barítono"),
    ("flute", "Flauta"),
    ("alto flute", "Flauta alto"),
    ("piccolo", "Flautín"),
    ("clarinet", "Clarinete"),
    ("bass clarinet", "Clarinete bajo"),
    ("music publisher", "Editorial"),
    ("artwork by", "Arte"),
    ("art director", "Dirección de arte"),
    ("photographer", "Fotografía"),
    ("video script editor", "Edición de guion de vídeo"),
    ("other", "Otros"),
];

/// A role without case, accents, spaces or punctuation, so «Co-Producer»,
/// «Co Producer» and the run-together roles of YouTube's descriptions
/// («Recordingarranger») read alike.
fn role_key(role: &str) -> String {
    fold(role)
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

/// The Spanish name of a known role.
fn spanish_role(role: &str) -> Option<&'static str> {
    let key = role_key(role);
    ROLE_LABELS
        .iter()
        .find(|(english, _)| role_key(english) == key)
        .map(|(_, spanish)| *spanish)
}

/// Spanish name of a role given by a source in English; other roles keep
/// the source's text.
fn role_label(role: &str) -> String {
    spanish_role(role).map_or_else(|| role.trim().to_owned(), str::to_owned)
}

/// The dialog's rows: each role under its Spanish name, roles that share a
/// name merged into one row, and every name listed once per row.
fn credit_rows(credits: &Credits) -> Vec<(String, Vec<&str>)> {
    let mut rows: Vec<(String, Vec<&str>)> = Vec::new();
    for role in &credits.roles {
        let label = role_label(&role.role);
        let index = match rows.iter().position(|(known, _)| *known == label) {
            Some(index) => index,
            None => {
                rows.push((label, Vec::new()));
                rows.len() - 1
            }
        };
        let names = &mut rows[index].1;
        for name in &role.names {
            if !names.contains(&name.as_str()) {
                names.push(name.as_str());
            }
        }
    }
    rows
}

/// Label of the menu entry and buttons that open the credits.
pub(crate) const CREDITS_LABEL: &str = "Ver créditos";

impl AppState {
    /// Open the credits dialog for `track`.
    pub(crate) fn show_credits(&self, track: &Track) {
        let mut open = self.credits_track;
        open.set(Some(track.clone()));
    }

    /// Credits of `track`: tags for library files, the provider (cached) for
    /// online media. Runs off the UI thread.
    pub(crate) async fn load_credits(&self, track: &Track) -> Result<Credits, String> {
        if crate::remote::is_remote(track) {
            let video_id =
                crate::remote::video_id_of(track).ok_or("Identidad en línea no válida")?;
            return self.online_credits(&video_id).await;
        }
        let core = self.services.core.clone();
        let id = track.id.clone();
        tokio::task::spawn_blocking(move || core.track_credits(&id))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }
}

#[component]
pub fn CreditsDialog(track: Track, onclose: EventHandler<()>) -> Element {
    let state = use_context::<AppState>();
    let loader = state.clone();
    let subject = track.clone();
    let credits = use_resource(move || {
        let state = loader.clone();
        let track = subject.clone();
        async move { state.load_credits(&track).await }
    });
    let loaded = credits.read().clone();
    rsx! {
        style { {include_str!("credits_ui.css")} }
        div { class: "modal-backdrop", onclick: move |_| onclose.call(()),
            div {
                class: "modal-panel credits-panel",
                role: "dialog",
                aria_modal: "true",
                aria_label: "Créditos de {track.title}",
                onclick: |event| event.stop_propagation(),
                onkeydown: move |event| {
                    if event.key() == Key::Escape {
                        event.stop_propagation();
                        onclose.call(());
                    }
                },
                div { class: "modal-header",
                    div { class: "credits-heading",
                        Thumb { src: artwork_src(&track), size: "lg", icon_size: Some(20) }
                        div { class: "credits-heading__meta",
                            span { class: "credits-heading__eyebrow", "Créditos" }
                            span { class: "modal-title", "{track.title}" }
                            span { class: "credits-heading__artist", "{track.artist}" }
                        }
                    }
                    button {
                        class: "btn-icon-sm",
                        title: "Cerrar",
                        aria_label: "Cerrar créditos",
                        onclick: move |_| onclose.call(()),
                        Icon { name: "close", size: 14 }
                    }
                }
                match loaded {
                    None => rsx! { p { class: "rust-loading", role: "status", "Buscando créditos…" } },
                    Some(Err(error)) => rsx! { p { class: "task-row__error", role: "status", "No se pudieron cargar los créditos: {error}" } },
                    Some(Ok(credits)) if credits.is_empty() => rsx! {
                        p { class: "credits-empty", "Esta canción no trae créditos. Las canciones de YouTube publicadas por distribuidoras suelen incluirlos; los archivos propios los toman de sus etiquetas." }
                    },
                    Some(Ok(credits)) => rsx! {
                        div { class: "credits-list",
                            for (label, names) in credit_rows(&credits) {
                                div {
                                    key: "{label}",
                                    class: "info-row",
                                    span { class: "info-row__label", "{label}" }
                                    span { class: "info-row__value", "{names.join(\", \")}" }
                                }
                            }
                            if let Some(label) = credits.label.as_ref() {
                                div {
                                    class: "info-row",
                                    span { class: "info-row__label", "Sello" }
                                    span { class: "info-row__value", "{label}" }
                                }
                            }
                            if let Some(released) = credits.released.as_ref() {
                                div {
                                    class: "info-row",
                                    span { class: "info-row__label", "Lanzamiento" }
                                    span { class: "info-row__value", "{released}" }
                                }
                            }
                            if let Some(provider) = credits.provider.as_ref() {
                                div {
                                    class: "info-row",
                                    span { class: "info-row__label", "Distribución" }
                                    span { class: "info-row__value", "{provider}" }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_roles_read_in_spanish_and_unknown_ones_pass_through() {
        assert_eq!(role_label("Composer Lyricist"), "Composición y letra");
        assert_eq!(role_label(" producer "), "Producción");
        assert_eq!(role_label("Performer"), "Interpretación");
        assert_eq!(role_label("Writer"), "Autoría");
        assert_eq!(role_label("Music Publisher"), "Editorial");
        assert_eq!(role_label(" Theremin "), "Theremin");
    }

    #[test]
    fn spacing_punctuation_and_run_together_forms_name_the_same_role() {
        for (variant, spanish) in [
            ("Co-Producer", "Coproducción"),
            ("Co Producer", "Coproducción"),
            ("Coproducer", "Coproducción"),
            ("Additionalproducer", "Producción adicional"),
            ("Additionalvocalist", "Voces adicionales"),
            ("Recordingarranger", "Arreglos de grabación"),
            ("Recordingsupervisor", "Supervisión de grabación"),
            ("Bassvocalist", "Voz bajo"),
            ("Drumprogrammer", "Programación de batería"),
            ("Drum Programmer", "Programación de batería"),
            ("Workarranger", "Arreglos de la obra"),
            ("Videoscripteditor", "Edición de guion de vídeo"),
            ("A& R", "A&R"),
            ("A&R Direction", "Dirección de A&R"),
            ("MIXING  ENGINEER", "Mezcla"),
        ] {
            assert_eq!(spanish_role(variant), Some(spanish), "{variant}");
        }
        assert_eq!(spanish_role("Theremin"), None);
    }

    #[test]
    fn every_role_has_its_own_key_and_a_name() {
        let mut keys = std::collections::HashSet::new();
        for (english, spanish) in ROLE_LABELS {
            assert!(keys.insert(role_key(english)), "{english}");
            assert!(!spanish.trim().is_empty(), "{english}");
        }
    }

    #[test]
    fn roles_that_share_a_spanish_name_share_one_row() {
        let role = |role: &str, names: &[&str]| yt_core::CreditRole {
            role: role.into(),
            names: names.iter().map(|name| (*name).to_owned()).collect(),
        };
        let credits = Credits {
            roles: vec![
                role("Mixer", &["Person A"]),
                role("Vocals", &["Person B"]),
                role("Mixing Engineer", &["Person C", "Person A"]),
                role("Theremin", &["Person D"]),
            ],
            ..Credits::default()
        };
        assert_eq!(
            credit_rows(&credits),
            [
                ("Mezcla".to_owned(), vec!["Person A", "Person C"]),
                ("Voz".to_owned(), vec!["Person B"]),
                ("Theremin".to_owned(), vec!["Person D"]),
            ]
        );
    }
}
