use crate::{
    icons::Icon,
    region::Detection,
    state::{size_label, AppState, Page},
};
use dioxus::prelude::*;
use std::{collections::BTreeMap, path::PathBuf, rc::Rc};
use yt_media::{CancellationToken, StreamKind};

#[component]
pub fn SettingsPage() -> Element {
    let state = use_context::<AppState>();
    let version = env!("CARGO_PKG_VERSION");
    let settings = state.settings.read().clone();
    let tracks = state.library.read().clone();
    let audio_count = tracks.iter().filter(|t| t.media_type == "audio").count();
    let total_size = tracks.iter().map(|t| t.size_bytes).sum();
    let mut mounts = use_signal(BTreeMap::<String, Rc<MountedData>>::new);
    let section = state.settings_section.read().clone();
    let navigation = state.clone();
    use_effect(move || {
        let selected = navigation.settings_section.read().clone();
        let selected = if selected == "plugin-scaffold" {
            "plugins"
        } else {
            selected.as_str()
        };
        if let Some(element) = mounts.read().get(selected).cloned() {
            spawn(async move {
                let _ = element.scroll_to(ScrollBehavior::Smooth).await;
            });
        }
    });
    let open = state.clone();
    let clean = state.clone();
    let scan = state.clone();
    rsx! {div{class:"page-shell page-shell--narrow",
      style { {include_str!("settings_ui.css")} }
      div{class:"page-header",h1{class:"page-title","Ajustes"}div{class:"page-subtitle","Los cambios se guardan automáticamente"}}
      section{class:"settings-section",
        SectionHeader{icon:"music",title:"Calidad de audio",description:"Para nuevas descargas MP3"}
        for (value,description) in [("128","Ahorro de espacio"),("192","Equilibrado"),("256","Alta calidad"),("320","Máxima calidad")] {
          Choice{active:settings.audio_quality==value,label:format!("{value} kbps"),description,key_name:"audio_quality",value}
        }
      }
      section{class:"settings-section",
        SectionHeader{icon:"video",title:"Calidad de vídeo",description:"Para nuevas descargas MP4"}
        for (value,label,description) in [("best","Máxima disponible","Usa la mejor fuente compatible"),("2160","2160p","4K"),("1080","1080p","Full HD"),("720","720p","Ahorro de espacio")] {
          Choice{active:settings.video_quality==value,label,description,key_name:"video_quality",value}
        }
        if !["best","2160","1080","720"].contains(&settings.video_quality.as_str()) {
          div{class:"settings-row",span{class:"settings-row__label",span{class:"settings-row__title","{settings.video_quality}p"}span{class:"settings-row__sub","Preferencia guardada"}}span{class:"settings-row__check active",Icon{name:"check",size:11}}}
        }
      }
      PlaybackSection { preferences: settings.playback.clone() }
      DiscoverySection { online: settings.discovery.online }
      LyricsSection { online: settings.lyrics.online }
      div{id:"region",onmounted:move|event|{mounts.write().insert("region".into(),event.data());},RegionSection { region: settings.region.clone() }}
      AppearanceSection { preferences: settings.appearance.clone() }
      PrivacySection {}
      section{class:"settings-section",
        SectionHeader{icon:"hard-drive",title:"Biblioteca",description:state.services.library_dir.display().to_string()}
        div{class:"settings-stat-grid",
          Stat{label:"Archivos",value:tracks.len().to_string()}
          Stat{label:"Canciones",value:audio_count.to_string()}
          Stat{label:"Espacio",value:size_label(total_size)}
        }
        button{class:"settings-row",onclick:move |_|crate::native::open_folder(&open,&open.services.library_dir),span{class:"settings-row__main",Icon{name:"folder",size:15}span{class:"settings-row__label",span{class:"settings-row__title",if cfg!(target_os="macos"){"Abrir en Finder"}else{"Abrir en Explorador"}}}}Icon{name:"chevron-right",size:13}}
        button{class:"settings-row",onclick:move |_|clean.clean_temporary_files(),span{class:"settings-row__main",Icon{name:"trash",size:15}span{class:"settings-row__label",span{class:"settings-row__title","Limpiar temporales"}span{class:"settings-row__sub","Elimina temporales obsoletos sin afectar a las descargas guardadas"}}}Icon{name:"chevron-right",size:13}}
        button{class:"settings-row",onclick:move |_|scan.scan_library(true),span{class:"settings-row__title","Actualizar biblioteca"}Icon{name:"chevron-right",size:13}}
      }
      div{id:"connection",onmounted:move|event|{mounts.write().insert("connection".into(),event.data());},ConnectionPanel{}}
      details{class:"settings-disclosure",open:settings.developer_mode,
        summary{span{Icon{name:"wrench",size:15}"Opciones avanzadas"}Icon{name:"chevron-right",size:14}}
        div{class:"settings-disclosure__content",
          Toggle{key_name:"show_advanced_tools",title:"Herramientas avanzadas",description:"Muestra Stems, fingerprint y controles técnicos",checked:settings.show_advanced_tools}
          Toggle{key_name:"developer_mode",title:"Modo desarrollador",description:"Activa plugins locales, logs, diagnósticos y el menú Desarrollo",checked:settings.developer_mode,tone:"warning"}
          if settings.show_advanced_tools {AdvancedOptions{}}
        }
      }
      if settings.developer_mode {
        div{id:"plugins",onmounted:move|event|{mounts.write().insert("plugins".into(),event.data());},crate::plugins::PluginPanel{}}
        crate::dj_analysis_ui::DjAnalysisPanel {}
      }
      if settings.developer_mode || section=="diagnostics" {
        div{id:"diagnostics",onmounted:move|event|{mounts.write().insert("diagnostics".into(),event.data());},DiagnosticsPanel{}}
      }
      div{id:"updates",onmounted:move|event|{mounts.write().insert("updates".into(),event.data());},crate::updates::UpdatePanel{}}
      div{class:"settings-footer","YT Ultimate {version}"}
    }}
}

#[component]
fn AppearanceSection(preferences: yt_core::AppearancePreferences) -> Element {
    let state = use_context::<AppState>();
    let covers = state.clone();
    rsx! {
        section {
            class: "settings-section",
            SectionHeader {
                icon: "palette",
                title: "Apariencia",
                description: "Colores y carátulas de la interfaz",
            }
            SwitchRow {
                title: "Color dinámico",
                description: "El acento y el fondo siguen la carátula de lo que suena",
                checked: preferences.dynamic_color,
                onchange: move |value| state.set_dynamic_color(value),
            }
            SwitchRow {
                title: "Carátulas de álbum",
                description: "Busca en YouTube la portada del disco de lo que escuchas y la muestra en lugar de la miniatura del vídeo",
                checked: preferences.album_artwork,
                onchange: move |value| covers.set_album_artwork(value),
            }
        }
    }
}

#[component]
pub(crate) fn SectionHeader(icon: String, title: String, description: String) -> Element {
    rsx! {div{class:"settings-section__header",div{class:"settings-section__icon",Icon{name:icon,size:15}}div{div{class:"settings-section__title","{title}"}div{class:"settings-section__sub","{description}"}}}}
}
#[component]
pub(crate) fn Stat(label: String, value: String) -> Element {
    rsx! {div{class:"settings-stat",div{class:"settings-stat__value","{value}"}div{class:"settings-stat__label","{label}"}}}
}
#[component]
fn Choice(
    active: bool,
    label: String,
    description: String,
    key_name: &'static str,
    value: &'static str,
) -> Element {
    let state = use_context::<AppState>();
    rsx! {button{r#type:"button",class:"settings-row",onclick:move |_|state.update_setting(key_name,serde_json::json!(value)),span{class:"settings-row__label",span{class:"settings-row__title","{label}"}span{class:"settings-row__sub","{description}"}}span{class:if active{"settings-row__check active"}else{"settings-row__check"},Icon{name:"check",size:11}}}}
}
/// One switch row; `onchange` receives the requested state.
#[component]
fn SwitchRow(
    title: String,
    description: String,
    checked: bool,
    onchange: EventHandler<bool>,
    #[props(default = "default".into())] tone: String,
) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "settings-row settings-row--toggle settings-row--{tone}",
            role: "switch",
            aria_checked: checked,
            onclick: move |_| onchange.call(!checked),
            span { class: "settings-row__label",
                span { class: "settings-row__title", "{title}" }
                span { class: "settings-row__sub", "{description}" }
            }
            span { class: if checked { "settings-switch active" } else { "settings-switch" }, span {} }
        }
    }
}

#[component]
fn Toggle(
    key_name: &'static str,
    title: String,
    description: String,
    checked: bool,
    #[props(default="default".into())] tone: String,
) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        SwitchRow {
            title,
            description,
            checked,
            tone,
            onchange: move |value: bool| state.update_setting(key_name, serde_json::json!(value)),
        }
    }
}

#[component]
fn PlaybackSection(preferences: yt_core::PlaybackPreferences) -> Element {
    let state = use_context::<AppState>();
    let crossfade = yt_core::PlaybackPreferences::CROSSFADE_SECONDS;
    let intensity = yt_core::PlaybackPreferences::DJ_INTENSITY;
    let autoplay = state.clone();
    let dj = state.clone();
    let skip = state.clone();
    let length = state.clone();
    let energy = state.clone();
    let marker = state.clone();
    let always = state.clone();
    let professional = preferences.dj_style.plans_mixes();
    let fusions = preferences.dj_style == yt_core::DjStyle::Mashup && state.renders_fusions();
    rsx! {
        section { class: "settings-section",
            SectionHeader { icon: "disc", title: "Reproducción", description: "Cómo sigue la música cuando termina una canción" }
            SwitchRow {
                title: "Reproducción automática",
                description: "Al terminar la cola, sigue con música parecida a la última canción",
                checked: preferences.autoplay,
                onchange: move |value| autoplay.set_autoplay(value),
            }
            SwitchRow {
                title: "Modo DJ",
                description: "Funde cada canción con la siguiente, como en una mezcla continua",
                checked: preferences.dj_mode,
                onchange: move |value| dj.set_dj_mode(value),
            }
            if preferences.dj_mode {
                for style in yt_core::DjStyle::ALL {
                    StyleChoice {
                        key: "{style:?}",
                        style,
                        active: preferences.dj_style == style,
                    }
                }
                if professional {
                    for mix in yt_core::MixLength::ALL {
                        LengthChoice {
                            key: "{mix:?}",
                            length: mix,
                            active: preferences.dj_length == mix,
                        }
                    }
                    SliderRow {
                        title: "Intensidad",
                        description: "De mezclas largas y suaves a cortes y filtros enérgicos",
                        aria_label: "Intensidad de las mezclas",
                        min: *intensity.start(),
                        max: *intensity.end(),
                        value: preferences.dj_intensity,
                        labels: intensity.clone().map(|value| crate::dj::intensity_label(value).to_owned()).collect::<Vec<_>>(),
                        invalid: format!("La intensidad va de {} a {}", intensity.start(), intensity.end()),
                        onchange: move |value| energy.edit_settings(|settings| settings.playback.dj_intensity = value),
                    }
                    SwitchRow {
                        title: "Mostrar la mezcla en la barra",
                        description: "Marca en la barra de tiempo dónde empieza la próxima mezcla y cuánto dura",
                        checked: preferences.dj_show_mix,
                        onchange: move |value| marker.edit_settings(|settings| settings.playback.dj_show_mix = value),
                    }
                    if fusions {
                        SwitchRow {
                            title: "Fusión IA en cada cambio",
                            description: "La IA toca cada cambio entre canciones, sin importar su tempo ni su tono",
                            checked: preferences.dj_always_fuse,
                            onchange: move |value| always.edit_settings(|settings| settings.playback.dj_always_fuse = value),
                        }
                        p {
                            class: "settings-attribution",
                            "{crate::dj::FUSION_ATTRIBUTION}"
                        }
                    }
                }
                SliderRow {
                    title: if professional { "Duración del fundido de respaldo" } else { "Duración del fundido" },
                    description: "Segundos en que suenan las dos canciones a la vez",
                    aria_label: "Duración del fundido en segundos",
                    min: *crossfade.start(),
                    max: *crossfade.end(),
                    value: preferences.crossfade_seconds,
                    labels: crossfade.clone().map(|seconds| format!("{seconds} s")).collect::<Vec<_>>(),
                    invalid: format!("La duración del fundido va de {} a {} segundos", crossfade.start(), crossfade.end()),
                    onchange: move |value| length.edit_settings(|settings| settings.playback.crossfade_seconds = value),
                }
                for curve in yt_core::CrossfadeCurve::ALL {
                    CurveChoice { key: "{curve:?}", curve, active: preferences.crossfade_curve == curve }
                }
                SwitchRow {
                    title: "Fundir también al saltar",
                    description: "Un fundido corto cuando cambias de canción tú",
                    checked: preferences.crossfade_on_skip,
                    onchange: move |value| skip.edit_settings(|settings| settings.playback.crossfade_on_skip = value),
                }
            }
        }
    }
}

/// One option of a group, checked when `active`; `onselect` picks it.
#[component]
fn OptionRow(
    label: String,
    description: String,
    active: bool,
    onselect: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            r#type: "button",
            class: "settings-row",
            onclick: move |_| onselect.call(()),
            span { class: "settings-row__label",
                span { class: "settings-row__title", "{label}" }
                span { class: "settings-row__sub", "{description}" }
            }
            span {
                class: if active { "settings-row__check active" } else { "settings-row__check" },
                Icon { name: "check", size: 11 }
            }
        }
    }
}

#[component]
fn CurveChoice(curve: yt_core::CrossfadeCurve, active: bool) -> Element {
    let state = use_context::<AppState>();
    let (label, description) = crate::dj::curve_label(curve);
    rsx! {
        OptionRow {
            label,
            description,
            active,
            onselect: move |_| state.edit_settings(|settings| settings.playback.crossfade_curve = curve),
        }
    }
}

#[component]
fn StyleChoice(style: yt_core::DjStyle, active: bool) -> Element {
    let state = use_context::<AppState>();
    let (label, description) = crate::dj::style_label(style);
    rsx! {
        OptionRow {
            label,
            description,
            active,
            onselect: move |_| state.edit_settings(|settings| settings.playback.dj_style = style),
        }
    }
}

#[component]
fn LengthChoice(length: yt_core::MixLength, active: bool) -> Element {
    let state = use_context::<AppState>();
    let (label, description) = crate::dj::length_label(length);
    rsx! {
        OptionRow {
            label,
            description,
            active,
            onselect: move |_| state.edit_settings(|settings| settings.playback.dj_length = length),
        }
    }
}

/// A slider over `min..=max` that shows `labels[value - min]`. The value is
/// parsed and checked against the bounds in Rust before `onchange` gets it;
/// anything else shows `invalid`.
#[component]
fn SliderRow(
    title: String,
    description: String,
    aria_label: String,
    min: u8,
    max: u8,
    value: u8,
    labels: Vec<String>,
    invalid: String,
    onchange: EventHandler<u8>,
) -> Element {
    let state = use_context::<AppState>();
    let mut dragging = use_signal(|| None::<u8>);
    let shown = dragging.read().unwrap_or(value);
    let label = labels
        .get(usize::from(shown.saturating_sub(min)))
        .cloned()
        .unwrap_or_default();
    rsx! {
        div {
            class: "settings-row settings-row--slider",
            span {
                class: "settings-row__label",
                span {
                    class: "settings-row__title",
                    "{title}"
                }
                span {
                    class: "settings-row__sub",
                    "{description}"
                }
            }
            div {
                class: "settings-slider",
                input {
                    r#type: "range",
                    aria_label: "{aria_label}",
                    min: "{min}",
                    max: "{max}",
                    step: "1",
                    value: "{shown}",
                    oninput: move |event| {
                        if let Ok(value) = event.value().parse::<u8>() {
                            if (min..=max).contains(&value) {
                                dragging.set(Some(value));
                            }
                        }
                    },
                    onchange: move |event| {
                        dragging.set(None);
                        match event.value().parse::<u8>() {
                            Ok(value) if (min..=max).contains(&value) => onchange.call(value),
                            _ => state.notify_warning(invalid.clone()),
                        }
                    },
                }
                span {
                    class: "settings-slider__value",
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn DiscoverySection(online: bool) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        section { class: "settings-section",
            SectionHeader { icon: "sparkles", title: "Inicio y recomendaciones", description: "De dónde salen las sugerencias de tu inicio y de la reproducción automática" }
            SwitchRow {
                title: "Recomendaciones en línea",
                description: "Busca en YouTube música relacionada con lo que escuchas. Desactívalo para usar solo tu biblioteca, sin conexión.",
                checked: online,
                onchange: move |value: bool| {
                    state.edit_settings(|settings| settings.discovery.online = value);
                    state.clear_autoplay();
                    state.refresh_autoplay();
                    state.refresh_home(true);
                },
            }
        }
    }
}

/// The zone used to pick nearby concerts: detected from the device's location
/// with the system's permission, or typed by the user.
#[component]
fn RegionSection(region: yt_core::RegionPreferences) -> Element {
    let state = use_context::<AppState>();
    let detection = *state.region.detection.read();
    let zone = [region.city.trim(), region.country.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let status = match (detection.message(), zone.is_empty()) {
        (Some(message), false) if detection != Detection::Detecting => {
            format!("{message} Se mantiene {zone}.")
        }
        (Some(message), _) => message.to_owned(),
        (None, false) => zone,
        (None, true) => "Se detecta al abrir los conciertos de un artista.".to_owned(),
    };
    let toggle = state.clone();
    let detect = state.clone();
    let permissions = state.clone();
    rsx! {
        section { class: "settings-section",
            SectionHeader {
                icon: "map-pin",
                title: "Conciertos cerca de ti",
                description: "Destaca los conciertos de tus artistas en tu zona. Tu zona se guarda solo en este equipo.",
            }
            SwitchRow {
                title: "Detectar mi zona",
                description: "Usa la ubicación del equipo, con tu permiso, para saber en qué ciudad estás. La ubicación no sale de este equipo: solo se guarda el nombre de tu ciudad.",
                checked: region.detect,
                onchange: move |value: bool| toggle.set_region_detection(value),
            }
            if region.detect {
                div { class: "settings-row",
                    span { class: "settings-row__label",
                        span { class: "settings-row__title", "Tu zona" }
                        span { class: "settings-row__sub", "{status}" }
                    }
                    div { class: "settings-row__actions",
                        if detection == Detection::Denied {
                            button {
                                class: "btn-secondary",
                                onclick: move |_| crate::native::open_location_settings(&permissions),
                                "Abrir ajustes de ubicación"
                            }
                        }
                        button {
                            class: "btn-secondary",
                            disabled: detection == Detection::Detecting,
                            onclick: move |_| detect.detect_region(),
                            Icon { name: "map-pin", size: 13 }
                            "Detectar ahora"
                        }
                    }
                }
            } else {
                TypedRegion { region }
            }
        }
    }
}

/// City and country typed by the user while detection is off.
#[component]
fn TypedRegion(region: yt_core::RegionPreferences) -> Element {
    let state = use_context::<AppState>();
    let mut city = use_signal(|| region.city.clone());
    let mut country = use_signal(|| region.country.clone());
    let save = move |state: AppState| {
        let value = yt_core::RegionPreferences {
            city: city.peek().trim().to_owned(),
            country: country.peek().trim().to_owned(),
            detect: false,
        };
        match value.validate() {
            Ok(()) => state.edit_settings(|settings| settings.region = value),
            Err(_) => state.notify_warning(format!(
                "Ciudad y país admiten hasta {} caracteres",
                yt_core::RegionPreferences::MAX_CHARS
            )),
        }
    };
    let save_city = state.clone();
    let save_country = state.clone();
    rsx! {
        div { class: "advanced-grid",
            label {
                span { "Ciudad" }
                input {
                    r#type: "text",
                    aria_label: "Ciudad",
                    placeholder: "Por ejemplo, Guadalajara",
                    maxlength: "{yt_core::RegionPreferences::MAX_CHARS}",
                    value: "{city}",
                    oninput: move |event| city.set(event.value()),
                    onblur: move |_| save(save_city.clone()),
                }
            }
            label {
                span { "País" }
                input {
                    r#type: "text",
                    aria_label: "País",
                    placeholder: "Por ejemplo, México",
                    maxlength: "{yt_core::RegionPreferences::MAX_CHARS}",
                    value: "{country}",
                    oninput: move |event| country.set(event.value()),
                    onblur: move |_| save(save_country.clone()),
                }
            }
        }
    }
}

#[component]
fn LyricsSection(online: bool) -> Element {
    let state = use_context::<AppState>();
    rsx! {
        section { class: "settings-section",
            SectionHeader { icon: "mic", title: "Letras", description: "Letras sincronizadas para cantar lo que suena" }
            SwitchRow {
                title: "Buscar letras en línea",
                description: "Al abrir la letra se busca en el catálogo libre LRCLIB con el título, el artista y la duración de la canción. Desactívalo para usar solo las letras de tus archivos (.lrc o etiquetas), sin conexión.",
                checked: online,
                onchange: move |value: bool| {
                    state.edit_settings(|settings| settings.lyrics.online = value);
                    state.reload_lyrics();
                },
            }
        }
    }
}

#[component]
fn PrivacySection() -> Element {
    let state = use_context::<AppState>();
    let mut confirming = use_signal(|| false);
    rsx! {
        section { class: "settings-section",
            SectionHeader { icon: "shield", title: "Privacidad", description: "Tu historial y tus favoritos se guardan solo en este equipo" }
            div { class: "settings-row",
                span { class: "settings-row__label",
                    span { class: "settings-row__title", "Borrar historial de escucha" }
                    span { class: "settings-row__sub", "Elimina lo que has escuchado, tus búsquedas y las recomendaciones derivadas. Tus favoritos y los artistas que sigues se conservan. No se puede deshacer." }
                }
                if *confirming.read() {
                    div { class: "settings-row__actions",
                        button { class: "btn-secondary", onclick: move |_| confirming.set(false), "Cancelar" }
                        button {
                            class: "btn-accent rust-danger-fill",
                            onclick: move |_| {
                                confirming.set(false);
                                state.erase_listening_history();
                            },
                            "Borrar"
                        }
                    }
                } else {
                    button { class: "btn-secondary", onclick: move |_| confirming.set(true), "Borrar…" }
                }
            }
        }
    }
}
#[component]
fn NumberSetting(
    name: &'static str,
    label: &'static str,
    value: u64,
    min: u64,
    max: u64,
) -> Element {
    let state = use_context::<AppState>();
    let mut draft = use_signal(|| value.to_string());
    use_effect(move || draft.set(value.to_string()));
    rsx! {label{span{"{label}"}input{r#type:"number",aria_label:label,value:"{draft}",min:"{min}",max:"{max}",oninput:move|event|draft.set(event.value()),onblur:move |_|{let parsed=draft.peek().parse::<u64>();match parsed{Ok(v) if (min..=max).contains(&v)=>state.update_setting(name,serde_json::json!(v)),_=>{draft.set(value.to_string());state.notify_warning(format!("{label}: escribe un valor entre {min} y {max}"));}}}}}}
}

#[component]
fn AdvancedOptions() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings.read().clone();
    let mut template = use_signal(|| settings.filename_template.clone());
    rsx! {
      div{class:"advanced-grid",
        NumberSetting{name:"max_concurrent_downloads",label:"Descargas simultáneas",value:settings.max_concurrent_downloads as u64,min:1,max:8}
        NumberSetting{name:"max_concurrent_processing",label:"Procesos pesados",value:settings.max_concurrent_processing as u64,min:1,max:4}
        label{class:"advanced-grid__wide",span{"Plantilla de archivo"}input{r#type:"text",aria_label:"Plantilla de archivo",value:"{template}",oninput:move|event|template.set(event.value()),onblur:{let state=state.clone();move |_|{let value=template.peek().trim().to_owned();let value=if value.is_empty(){"{title}".into()}else{value};match crate::downloads::validate_filename_template(&value){Ok(())=>state.update_setting("filename_template",serde_json::json!(value)),Err(error)=>state.notify_error(error)}}}}small{{"{title} · {channel} · {video_id} · {media_type} · {quality}"}}}
        label{span{"Descarga predeterminada"}select{value:"{settings.default_download_type}",aria_label:"Descarga predeterminada",onchange:{let state=state.clone();move|event|state.update_setting("default_download_type",serde_json::json!(event.value()))},option{value:"audio","Audio MP3"}option{value:"video","Vídeo MP4"}}}
        label{span{"Idioma de subtítulos"}select{value:"{settings.language}",aria_label:"Idioma de subtítulos",onchange:{let state=state.clone();move|event|state.update_setting("language",serde_json::json!(event.value()))},option{value:"es","Español"}option{value:"en","English"}}}
      }
      Toggle{key_name:"auto_thumbnail",title:"Guardar carátulas",description:"Incluye la imagen del vídeo en las nuevas descargas",checked:settings.auto_thumbnail}
      Toggle{key_name:"preserve_source_metadata",title:"Conservar metadatos",description:"Mantiene títulos, artistas y carátulas al descargar y exportar audio",checked:settings.preserve_source_metadata}
      Toggle{key_name:"download_subtitles",title:"Descargar subtítulos",description:"Guarda los subtítulos disponibles junto al archivo",checked:settings.download_subtitles}
      div{class:"advanced-grid",label{class:"advanced-grid__wide",span{"Importación"}select{aria_label:"Importación",value:"{settings.import_mode}",onchange:{let state=state.clone();move|event|state.update_setting("import_mode",serde_json::json!(event.value()))},option{value:"copy","Copiar a la biblioteca"}option{value:"reference","Usar ubicación original"}}}}
      Toggle{key_name:"scan_library_on_startup",title:"Buscar archivos al iniciar",description:"Añade nuevos archivos de la carpeta de biblioteca",checked:settings.scan_library_on_startup}
    }
}

#[component]
fn ConnectionPanel() -> Element {
    let state = use_context::<AppState>();
    let settings = state.settings.read().clone();
    let mut browser = use_signal(|| {
        settings
            .extra
            .get("youtube_cookies_browser")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let mut profile = use_signal(|| {
        settings
            .extra
            .get("youtube_browser_profile")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let mut proxy = use_signal(|| {
        settings
            .extra
            .get("youtube_proxy")
            .or_else(|| settings.extra.get("proxy"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    let cookie_file = settings
        .extra
        .get("youtube_cookie_file")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut busy = use_signal(|| false);
    let mut connection = use_signal(String::new);
    let mut revision = use_signal(|| 0u64);
    let mut diagnostic = use_signal(String::new);
    let mut testing = use_signal(|| None::<CancellationToken>);
    let active_session = state.services.browser_session_info();
    let connect_state = state.clone();
    let disconnect_state = state.clone();
    let file_state = state.clone();
    let remove_state = state.clone();
    let test_state = state.clone();
    rsx! {section{class:"settings-section",
      SectionHeader{icon:"activity",title:"Conexión con YouTube",description:"Si YouTube pide iniciar sesión, elige las cookies de tu navegador o un archivo Netscape."}
      div{class:"advanced-grid",
        label{class:"advanced-grid__wide",span{"Cookies del navegador"}select{aria_label:"Cookies del navegador",value:"{browser}",disabled:*busy.read(),onchange:move|event|browser.set(event.value()),option{value:"","Sin navegador"}for name in ["chrome","chromium","edge","firefox","safari","brave","opera","vivaldi","whale"]{option{value:name,disabled:name=="safari"&&!cfg!(target_os="macos"),"{name}"}}}}
        label{class:"advanced-grid__wide",span{"Perfil del navegador"}input{r#type:"text",aria_label:"Perfil del navegador",placeholder:"Predeterminado",value:"{profile}",readonly:true}}
        button{class:"btn-secondary",disabled:*busy.read(),onclick:move |_|{spawn(async move{if let Some(folder)=rfd::AsyncFileDialog::new().set_title("Elegir perfil del navegador").pick_folder().await{profile.set(folder.path().to_string_lossy().into_owned());}});},Icon{name:"folder",size:13}"Elegir perfil"}
        button{class:"btn-secondary",disabled:*busy.read()||profile.read().is_empty(),onclick:move |_|profile.set(String::new()),"Perfil predeterminado"}
        button{class:"btn-secondary",disabled:*busy.read()||browser.read().is_empty(),onclick:move |_|{
          let state=connect_state.clone();let selected=browser.peek().clone();let path=profile.peek().clone();let request=*revision.peek()+1;revision.set(request);busy.set(true);connection.set("Esperando autorización del sistema…".into());
          spawn(async move{
            let source=selected.clone();let source_path=path.clone();
            let result=tokio::task::spawn_blocking(move||yt_media::read_browser_cookies(&source,if source_path.is_empty(){None}else{Some(std::path::Path::new(&source_path))})).await;
            if *revision.peek()!=request{return;}busy.set(false);
            match result {Ok(Ok(session))=>{
              let mut values=state.settings.peek().clone();values.extra.insert("youtube_cookie_file".into(),serde_json::json!(""));values.extra.insert("youtube_cookies_browser".into(),serde_json::json!(selected));values.extra.insert("youtube_browser_profile".into(),serde_json::json!(path));state.save_settings(values);
              match state.services.set_browser_session(session){Ok(())=>connection.set("Conectado durante esta sesión. Las cookies no se guardan en disco.".into()),Err(error)=>connection.set(error)}
            },Ok(Err(error))=>connection.set(error.to_string()),Err(_)=>connection.set("La conexión con el navegador se interrumpió.".into())}
          });
        },if *busy.read(){"Conectando…"}else{"Conectar"}}
        if *busy.read(){button{class:"btn-secondary",onclick:move |_|{revision+=1;busy.set(false);connection.set("Conexión cancelada; no se usará su resultado.".into());},"Cancelar"}}
        if active_session.is_some(){button{class:"btn-secondary",onclick:move |_|{match disconnect_state.services.clear_browser_session(){Ok(())=>{disconnect_state.update_setting("youtube_cookies_browser",serde_json::json!(""));connection.set("Sesión del navegador desconectada.".into());},Err(error)=>connection.set(error)}},"Desconectar navegador"}}
        if let Some((name,count))=active_session{p{class:"advanced-grid__wide settings-row__sub","{name}: {count} cookies de YouTube autorizadas"}}
        if !connection.read().is_empty(){p{class:"advanced-grid__wide settings-row__sub",role:"status","{connection}"}}
        label{class:"advanced-grid__wide",span{"Archivo de cookies"}input{r#type:"text",aria_label:"Archivo de cookies",value:"{cookie_file}",readonly:true,placeholder:"Ruta al archivo cookies.txt"}}
        button{class:"btn-secondary",onclick:move |_|{let state=file_state.clone();spawn(async move{if let Some(file)=rfd::AsyncFileDialog::new().set_title("Elegir archivo de cookies Netscape").add_filter("Cookies",&["txt"]).pick_file().await{if let Err(error)=state.services.clear_browser_session(){state.notify_error(error);return;}let mut values=state.settings.peek().clone();values.extra.insert("youtube_cookie_file".into(),serde_json::json!(file.path().to_string_lossy()));values.extra.insert("youtube_cookies_browser".into(),serde_json::json!(""));state.save_settings(values);browser.set(String::new());connection.set(String::new());}});},Icon{name:"folder",size:13}"Elegir archivo"}
        button{class:"btn-secondary",disabled:cookie_file.is_empty(),onclick:move |_|remove_state.update_setting("youtube_cookie_file",serde_json::json!("")),"Quitar cookies"}
        NumberSetting{name:"download_retries",label:"Reintentos de descarga",value:settings.download_retries as u64,min:0,max:10}
        NumberSetting{name:"download_socket_timeout",label:"Espera de red (segundos)",value:settings.download_socket_timeout as u64,min:1,max:120}
        label{class:"advanced-grid__wide",span{"Proxy HTTP(S)"}input{r#type:"password",aria_label:"Proxy HTTP(S)",placeholder:"Sin proxy",value:"{proxy}",oninput:move|event|proxy.set(event.value()),onblur:{let state=state.clone();move |_|state.update_setting("youtube_proxy",serde_json::json!(proxy.peek().trim().to_owned()))}}}
      }
      div{class:"advanced-grid",
        button{class:"btn-secondary",disabled:testing.read().is_some(),onclick:move |_|{
          let state=test_state.clone();let cancel=CancellationToken::new();testing.set(Some(cancel.clone()));diagnostic.set("Comprobando búsqueda y acceso al audio…".into());
          spawn(async move{
            let result=async{let youtube=state.services.youtube_client().map_err(|e|e.to_string())?;tokio::select!{_ = cancel.cancelled()=>Err("Comprobación cancelada".to_owned()),result=async{let results=youtube.search("Big Buck Bunny Blender",1).await.map_err(|e|e.to_string())?;if results.is_empty(){return Err("La búsqueda no devolvió resultados".into());}youtube.resolve("aqz-KE-bpKQ",StreamKind::Audio).await.map_err(|e|e.to_string())?;Ok("Conexión disponible: búsqueda y fuente de audio verificadas.".to_owned())}=>result}}.await;
            diagnostic.set(result.unwrap_or_else(|e|format!("No se pudo verificar la conexión: {e}")));testing.set(None);
          });
        },"Probar conexión"}
        if let Some(token)=testing.read().clone(){button{class:"btn-secondary",onclick:move |_|token.cancel(),"Cancelar comprobación"}}
        if !diagnostic.read().is_empty(){p{class:"advanced-grid__wide settings-row__sub",role:"status","{diagnostic}"}}
      }
    }}
}

fn model_available() -> bool {
    #[cfg(any(
        feature = "native-stems",
        all(feature = "metal-stems", target_os = "macos")
    ))]
    {
        crate::processing::model_path().is_ok_and(|path| path.is_file())
    }
    #[cfg(not(any(
        feature = "native-stems",
        all(feature = "metal-stems", target_os = "macos")
    )))]
    {
        false
    }
}
fn diagnostic_snapshot(state: &AppState) -> serde_json::Value {
    let jobs = state.services.core.jobs().unwrap_or_default();
    serde_json::json!({"application":"YT Ultimate","version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,
      "generated_at":chrono::Utc::now().to_rfc3339(),"engines":{"audio_output":state.services.audio.is_ok(),"youtube_resolver":true,"audio_conversion":true,"video_mux":true,"separator_model":model_available()},
      "library":{"tracks":state.library.peek().len(),"playlists":state.playlists.peek().len()},"jobs":{"active":jobs.iter().filter(|j|!j.status.is_terminal()).count(),"errors":jobs.iter().filter(|j|j.status==yt_core::JobStatus::Error).count()},"connection":{"browser_session":state.services.browser_session_info().is_some(),"cookie_file_configured":state.settings.peek().extra.get("youtube_cookie_file").and_then(serde_json::Value::as_str).is_some_and(|v|!v.is_empty())}})
}
fn write_diagnostics(
    profile: &std::path::Path,
    value: &serde_json::Value,
) -> Result<PathBuf, String> {
    use std::io::Write;
    let folder = profile.join("logs");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join("native-diagnostics.json");
    let mut temporary = tempfile::NamedTempFile::new_in(&folder).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(&path).map_err(|e| e.error.to_string())?;
    Ok(path)
}
#[component]
fn DiagnosticsPanel() -> Element {
    let state = use_context::<AppState>();
    let mut jobs = use_signal(|| state.services.core.jobs().unwrap_or_default());
    let polling = state.clone();
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if *polling.page.peek() == Page::Settings {
                    if let Ok(value) = polling.services.core.jobs() {
                        jobs.set(value);
                    }
                }
            }
        });
    });
    let rows = jobs
        .read()
        .iter()
        .filter(|job| !job.status.is_terminal())
        .cloned()
        .collect::<Vec<_>>();
    let model = model_available();
    let logs = state.clone();
    let refresh = state.clone();
    rsx! {section{class:"settings-section",
      SectionHeader{icon:"activity",title:"Diagnóstico",description:format!("YT Ultimate {} · {}",env!("CARGO_PKG_VERSION"),std::env::consts::OS)}
      div{class:"engine-grid",for(name,available)in[("Reproducción de audio",state.services.audio.is_ok()),("Conexión YouTube",state.services.youtube_client().is_ok()),("Conversión de audio",true),("Vídeo MP4",true),("Separador",model)]{div{class:"engine-row",Icon{name:"gauge",size:13}span{"{name}"}span{class:if available{"engine-ok"}else{"engine-missing"},if available{"Disponible"}else{"No disponible"}}}}}
      if !model {p{class:"settings-row__sub",if crate::processing::SEPARATOR_INCLUDED{"No se encontró el modelo de separación. Reinstala la edición Full para restaurarlo."}else{"Esta edición no incluye el separador. Está disponible en la edición Full."}}}
      if !rows.is_empty(){div{class:"settings-empty","Procesos en curso"}for job in rows{div{class:"settings-row",span{class:"settings-row__label",span{class:"settings-row__title","{job.kind}"}span{class:"settings-row__sub","{job.message} · {job.progress.round()}%"}}button{class:"btn-secondary",onclick:{let state=state.clone();let id=job.id.clone();move |_|{let token=state.cancellations.peek().get(&id).cloned();if let Some(token)=token{token.cancel();}else{state.notify_warning("Este trabajo ya no está activo");}}},"Cancelar"}}}}
      button{class:"settings-row",onclick:move |_|{match write_diagnostics(&logs.services.profile,&diagnostic_snapshot(&logs)){Ok(path)=>crate::native::open_folder(&logs,path.parent().unwrap_or(&logs.services.profile)),Err(error)=>logs.notify_error(error)}},span{class:"settings-row__main",Icon{name:"folder",size:15}span{class:"settings-row__label",span{class:"settings-row__title","Abrir logs"}span{class:"settings-row__sub","Incluye el diagnóstico actual sin cookies ni enlaces privados"}}}Icon{name:"chevron-right",size:13}}
      button{class:"settings-row",onclick:move |_|{jobs.set(refresh.services.core.jobs().unwrap_or_default());refresh.notify_success("Diagnóstico actualizado");},span{class:"settings-row__title","Actualizar diagnóstico"}Icon{name:"chevron-right",size:13}}
    }}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playback() -> Element {
        let state = use_context::<AppState>();
        let preferences = state.settings.read().playback.clone();
        rsx! { PlaybackSection { preferences } DiscoverySection { online: true } PrivacySection {} }
    }

    #[test]
    fn playback_preferences_render_from_their_bounds_and_hide_dj_details_when_off() {
        let off = crate::test_support::render(playback, |_| {});
        assert!(off.contains("Reproducción automática"));
        assert!(off.contains("Modo DJ"));
        assert!(!off.contains("Duración del fundido"));
        assert!(off.contains("Recomendaciones en línea"));
        assert!(off.contains("Borrar historial de escucha"));
        let on = crate::test_support::render(playback, |state| state.set_dj_mode(true));
        assert!(on.contains("Duración del fundido"));
        assert!(on.contains("Profesional") && on.contains("Clásico"));
        assert!(on.contains("Intensidad") && on.contains("Equilibrada"));
        for length in yt_core::MixLength::ALL {
            assert!(on.contains(crate::dj::length_label(length).0));
        }
        let intensity = yt_core::PlaybackPreferences::DJ_INTENSITY;
        assert!(on.contains(&format!("max=\"{}\"", intensity.end())));
        let classic = crate::test_support::render(playback, |state| {
            state.set_dj_mode(true);
            state.edit_settings(|settings| settings.playback.dj_style = yt_core::DjStyle::Classic);
        });
        assert!(
            !classic.contains("Intensidad"),
            "the classic style has no mix settings"
        );
        assert!(on.contains("Mashup con IA") && on.contains("Mostrar la mezcla en la barra"));
        let bounds = yt_core::PlaybackPreferences::CROSSFADE_SECONDS;
        assert!(on.contains(&format!("min=\"{}\"", bounds.start())));
        assert!(on.contains(&format!("max=\"{}\"", bounds.end())));
        for curve in yt_core::CrossfadeCurve::ALL {
            assert!(on.contains(crate::dj::curve_label(curve).0));
        }
    }

    fn region() -> Element {
        let state = use_context::<AppState>();
        let region = state.settings.read().region.clone();
        rsx! { RegionSection { region } }
    }

    #[test]
    fn the_region_is_detected_by_default_and_typed_when_detection_is_off() {
        let fresh = crate::test_support::render(region, |_| {});
        assert!(fresh.contains("Detectar mi zona"));
        assert!(fresh.contains("Se detecta al abrir los conciertos de un artista."));
        assert!(fresh.contains("Detectar ahora"));
        assert!(!fresh.contains("aria-label=\"Ciudad\""));
        let denied = crate::test_support::render(region, |state| {
            state.edit_settings(|settings| {
                settings.region.city = "Monterrey".into();
                settings.region.country = "México".into();
            });
            state.region.detection.set(Detection::Denied);
        });
        assert!(denied.contains("Se mantiene Monterrey, México."));
        assert!(denied.contains("Abrir ajustes de ubicación"));
        let typed = crate::test_support::render(region, |state| state.set_region_detection(false));
        assert!(typed.contains("aria-label=\"Ciudad\""));
        assert!(!typed.contains("Detectar ahora"));
    }
    use crate::state::{use_app_state, Services};
    use dioxus::dioxus_core::{AttributeValue, Mutation};
    use std::cell::RefCell;
    type Capture = Rc<RefCell<Option<AppState>>>;
    fn harness() -> Element {
        let services = use_context::<Rc<Services>>();
        let capture = use_context::<Capture>();
        let state = use_app_state(services);
        *capture.borrow_mut() = Some(state.clone());
        use_context_provider(|| state);
        rsx! {SettingsPage{}}
    }
    #[tokio::test]
    async fn settings_restore_legacy_controls_and_validate_number_edits_without_borrow_panics() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut settings = services.core.settings().unwrap();
        settings.show_advanced_tools = true;
        settings.developer_mode = true;
        services.core.save_settings(&settings).unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services.clone()));
        dom.insert_any_root_context(Box::new(capture.clone()));
        let mutations = dom.rebuild_to_vec();
        let html = dioxus_ssr::render(&dom);
        for label in [
            "Calidad de audio",
            "Calidad de vídeo",
            "Máxima disponible",
            "2160p",
            "Biblioteca",
            "Cookies del navegador",
            "firefox",
            "whale",
            "Reintentos de descarga",
            "Espera de red (segundos)",
            "Procesos pesados",
            "Plantilla de archivo",
            "Diagnóstico",
            "Abrir logs",
            "Crear plugin",
        ] {
            assert!(html.contains(label), "missing {label}");
        }
        assert_eq!(
            services.core.settings().unwrap().max_concurrent_downloads,
            3
        );
        let input = mutations
            .edits
            .iter()
            .find_map(|edit| match edit {
                Mutation::SetAttribute {
                    name: "aria-label",
                    value: AttributeValue::Text(label),
                    id,
                    ..
                } if label == "Reintentos de descarga" => Some(*id),
                _ => None,
            })
            .unwrap();
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let event: Event<dyn std::any::Any> = Event::new(
            Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedFormData {
                    value: "999".into(),
                    values: Vec::new(),
                    valid: false,
                },
            ))),
            true,
        );
        dom.runtime().handle_event("input", event, input);
        dom.render_immediate_to_vec();
        let blur: Event<dyn std::any::Any> = Event::new(
            Rc::new(dioxus::html::PlatformEventData::new(Box::new(
                dioxus::html::SerializedFocusData::default(),
            ))),
            true,
        );
        dom.runtime().handle_event("blur", blur, input);
        dom.render_immediate_to_vec();
        assert_eq!(services.core.settings().unwrap().download_retries, 2);
        assert!(capture
            .borrow()
            .as_ref()
            .unwrap()
            .last_notice()
            .unwrap()
            .contains("entre 0 y 10"));
    }
    #[tokio::test]
    async fn diagnostic_report_never_serializes_connection_credentials_or_private_urls() {
        let profile = tempfile::tempdir().unwrap();
        let services = Rc::new(Services::for_test(profile.path()));
        let mut settings = services.core.settings().unwrap();
        settings.extra.insert(
            "youtube_proxy".into(),
            serde_json::json!("https://user:secret-value@private.example"),
        );
        settings.extra.insert(
            "youtube_cookie_file".into(),
            serde_json::json!("/private/secret-file.txt"),
        );
        services.core.save_settings(&settings).unwrap();
        let capture: Capture = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new(harness);
        dom.insert_any_root_context(Box::new(services));
        dom.insert_any_root_context(Box::new(capture.clone()));
        dom.rebuild_in_place();
        let state = capture.borrow().as_ref().unwrap().clone();
        let report = diagnostic_snapshot(&state);
        let path = write_diagnostics(profile.path(), &report).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        for secret in ["secret-value", "secret-file", "private.example"] {
            assert!(!text.contains(secret));
        }
        assert_eq!(report["connection"]["cookie_file_configured"], true);
    }
}
