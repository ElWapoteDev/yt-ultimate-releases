//! Song credits of art tracks, the recordings that labels and distributors
//! deliver: the description YouTube writes for them («Provided to YouTube by
//! …») and their YouTube Music credits page.
//!
//! The generator emits blank-line separated blocks in a fixed order, always in
//! English: provider, «Title · Artists», album, then optional «℗ …» and
//! «Released on: YYYY-MM-DD» lines and «Role, Role: Name, Name» lines. The
//! credits page lists performers, writers and producers, one name per line,
//! under section titles in the session's language; descriptions often omit
//! performers and writers.

use crate::{
    catalog::{find_key, joined, runs, Service},
    youtube::RequestMessages,
    CancellationToken, MediaError, Result, YoutubeClient,
};
use serde_json::{json, Value};
use yt_core::{
    fold_words, CreditRole, Credits, MAX_CREDIT_CHARS, MAX_CREDIT_NAMES, MAX_CREDIT_ROLES,
};

const PROVIDER_PREFIX: &str = "Provided to YouTube by ";
const LABEL_MARK: char = '℗';
const RELEASED_PREFIX: &str = "Released on:";
/// Separates roles from names, and entries within each side.
const ROLE_SEPARATOR: &str = ": ";
const LIST_SEPARATOR: &str = ", ";
/// Provider, «Title · Artists» and album precede every credit line; titles
/// and albums may contain «: » themselves.
const LEADING_BLOCKS: usize = 3;
/// Single-word roles the generator runs together, e.g. «Composer Lyricist».
const JOINED_ROLES: [&str; 7] = [
    "Composer",
    "Lyricist",
    "Producer",
    "Arranger",
    "Writer",
    "Songwriter",
    "Author",
];
/// Placeholder role of sources that name none; it says nothing about the person.
const UNKNOWN_ROLE: &str = "Unknown";
/// Watch-page locations of the full description, newest layout first.
const DESCRIPTION_PATHS: [&str; 2] = [
    "/videoSecondaryInfoRenderer/attributedDescription/content",
    "/expandableVideoDescriptionBodyRenderer/attributedDescriptionBodyText/content",
];
const WATCH_RESULTS: &str = "/contents/twoColumnWatchNextResults/results/results/contents";
const DESCRIPTION_PANEL: &str =
    "/engagementPanelSectionListRenderer/content/structuredDescriptionContentRenderer/items";

/// Browse id of an art track's YouTube Music credits page: this prefix and
/// the video id.
const TRACK_CREDITS_PREFIX: &str = "MPTC";
const TRACK_CREDITS_DIALOG: &str = "dismissableDialogRenderer";
const TRACK_CREDITS_SECTION: &str = "dismissableDialogContentSectionRenderer";
/// Separates the names of one credits-page section.
const NAME_SEPARATOR: char = '\n';
/// Credits-page section titles, folded, and the English role each credits.
/// Each list holds the titles YouTube served on 2026-11-11 in the languages
/// this application serves: Spanish, English, Portuguese (Brazil and
/// Portugal), French (France and Canada), Italian and German. Other titles,
/// such as the source of the metadata, are skipped.
const TRACK_CREDIT_ROLES: [(&str, &[&str]); 3] = [
    (
        "Performer",
        &[
            "interpretada por",
            "performed by",
            "interpretacao de",
            "interprete s",
            "interpretee par",
            "eseguito da",
            "interpretiert von",
        ],
    ),
    (
        "Writer",
        &[
            "escrita por",
            "written by",
            "composicao de",
            "composta por",
            "auteur s compositeur s",
            "ecrite par",
            "scritto da",
            "geschrieben von",
        ],
    ),
    (
        "Producer",
        &[
            "producida por",
            "produced by",
            "produzido por",
            "produzida por",
            "producteur s",
            "produite par",
            "prodotto da",
            "produziert von",
        ],
    ),
];
/// Description roles that detail whom a credits-page role summarizes. A page
/// name the description already credits in one of them is not repeated, so
/// «Escrita por» adds only the writers that the composer and lyricist lines
/// omit.
const DETAILED_ROLES: [(&str, &[&str]); 2] = [
    (
        "Performer",
        &[
            "Associated Performer",
            "Main Artist",
            "Featured Artist",
            "Vocals",
        ],
    ),
    ("Writer", &["Composer", "Lyricist", "Songwriter", "Author"]),
];

const CREDITS_REQUEST: RequestMessages = RequestMessages {
    too_large: "La página del vídeo supera el límite admitido.",
    invalid: "La página del vídeo no entregó datos válidos.",
    timeout: "La consulta de créditos tardó demasiado.",
};

impl YoutubeClient {
    /// Credits of the recording `video_id` (an id or a YouTube link). One
    /// watch-next request reads the description YouTube auto-generates for
    /// art tracks; only for those, one YouTube Music request reads the
    /// track's credits page, whose performers, writers and producers come
    /// first, without the names the description already credits in a detailed
    /// role such as composer or lyricist, followed by the description's roles.
    /// When that page fails, the description's credits are returned. Music
    /// videos and other uploads have no such description and return empty
    /// credits. Every value satisfies [`Credits::validate`]: roles and names
    /// are bounded and deduplicated.
    pub async fn credits(&self, video_id: &str, cancel: CancellationToken) -> Result<Credits> {
        let id = crate::youtube::video_id(video_id)?;
        let response = self
            .innertube(
                Service::Web,
                "/next",
                json!({ "videoId": id }),
                &CREDITS_REQUEST,
                &cancel,
            )
            .await?;
        let Some(credits) = watch_description(&response).and_then(parse_credits) else {
            return Ok(Credits::default());
        };
        let request = json!({ "browseId": format!("{TRACK_CREDITS_PREFIX}{id}") });
        match self
            .innertube(
                Service::Music,
                "/browse",
                request,
                &CREDITS_REQUEST,
                &cancel,
            )
            .await
        {
            Ok(page) => Ok(with_track_credits(credits, &page)),
            Err(MediaError::Cancelled) => Err(MediaError::Cancelled),
            Err(_) => Ok(credits),
        }
    }
}

/// The full description of a watch-next response.
fn watch_description(response: &Value) -> Option<&str> {
    let results = response.pointer(WATCH_RESULTS).and_then(Value::as_array);
    let panels = response
        .get("engagementPanels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|panel| panel.pointer(DESCRIPTION_PANEL)?.as_array())
        .flatten();
    results
        .into_iter()
        .flatten()
        .chain(panels)
        .find_map(|item| {
            DESCRIPTION_PATHS
                .iter()
                .find_map(|path| item.pointer(path)?.as_str())
        })
}

/// Credits of an auto-generated description, which only art tracks have;
/// `None` for any other text.
pub(crate) fn parse_credits(description: &str) -> Option<Credits> {
    let description = description.replace("\r\n", "\n");
    let mut blocks = description.split("\n\n").map(str::trim);
    let provider = blocks.next()?.strip_prefix(PROVIDER_PREFIX)?;
    let mut credits = Credits {
        provider: bounded(unquoted(provider)),
        ..Credits::default()
    };
    for line in blocks.skip(LEADING_BLOCKS - 1).flat_map(str::lines) {
        let line = line.trim();
        if line.starts_with(LABEL_MARK) {
            credits.label = credits.label.take().or_else(|| bounded(line));
        } else if let Some(date) = line.strip_prefix(RELEASED_PREFIX) {
            credits.released = release_date(date.trim()).map(str::to_owned);
        } else if let Some((roles, names)) = line.split_once(ROLE_SEPARATOR) {
            add_roles(&mut credits.roles, roles, names);
        }
    }
    Some(credits)
}

/// `credits` with the roles of a YouTube Music credits page first, in page
/// order, and its own roles merged after them; page names that `credits`
/// already name in a detailed role are left out.
fn with_track_credits(credits: Credits, page: &Value) -> Credits {
    let mut roles = Vec::new();
    for mut role in track_credit_roles(page) {
        role.names
            .retain(|name| !detailed(&credits.roles, &role.role, name));
        add_names(&mut roles, role.role, &role.names);
    }
    for role in credits.roles {
        add_names(&mut roles, role.role, &role.names);
    }
    Credits { roles, ..credits }
}

/// Whether `roles` credit `name` in a role that details the page role `summary`.
fn detailed(roles: &[CreditRole], summary: &str, name: &str) -> bool {
    let details = DETAILED_ROLES
        .iter()
        .find(|(role, _)| *role == summary)
        .map_or(&[][..], |(_, details)| *details);
    roles.iter().any(|role| {
        details.contains(&role.role.as_str()) && role.names.iter().any(|known| known == name)
    })
}

/// Performers, writers and producers of a YouTube Music credits page, in page
/// order; sections are told apart by their folded titles only.
fn track_credit_roles(page: &Value) -> Vec<CreditRole> {
    let sections = find_key(page.get("onResponseReceivedActions"), TRACK_CREDITS_DIALOG)
        .and_then(|dialog| dialog.get("sections"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|section| section.get(TRACK_CREDITS_SECTION));
    let mut roles = Vec::new();
    for section in sections {
        let title = fold_words(&joined(&runs(section.get("title"))));
        let Some((role, _)) = TRACK_CREDIT_ROLES
            .iter()
            .find(|(_, titles)| titles.contains(&title.as_str()))
        else {
            continue;
        };
        let names: Vec<String> = joined(&runs(section.get("subtitle")))
            .split(NAME_SEPARATOR)
            .filter_map(bounded)
            .collect();
        add_names(&mut roles, (*role).to_owned(), &names);
    }
    roles
}

/// Text with single spaces that satisfies the credit bounds of `yt_core`.
fn bounded(text: &str) -> Option<String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()
        && text.chars().count() <= MAX_CREDIT_CHARS
        && !text.chars().any(char::is_control))
    .then_some(text)
}

/// Some distributors quote their own name: «'Label Name'».
fn unquoted(text: &str) -> &str {
    let text = text.trim();
    ['\'', '"']
        .into_iter()
        .find_map(|quote| text.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(text)
}

/// `YYYY-MM-DD` with a real month and day number, as the generator prints it.
fn release_date(text: &str) -> Option<&str> {
    let mut fields = text.splitn(3, '-');
    let (year, month, day) = (fields.next()?, fields.next()?, fields.next()?);
    let digits = |field: &str, length: usize| {
        field.len() == length && field.bytes().all(|byte| byte.is_ascii_digit())
    };
    let in_range = |field: &str, maximum: u8| {
        field
            .parse::<u8>()
            .is_ok_and(|v| (1..=maximum).contains(&v))
    };
    (digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && in_range(month, 12)
        && in_range(day, 31))
    .then_some(text)
}

/// Add `names` to every role of `roles` but the placeholder role.
fn add_roles(credits: &mut Vec<CreditRole>, roles: &str, names: &str) {
    let names: Vec<String> = names.split(LIST_SEPARATOR).filter_map(bounded).collect();
    for role in roles
        .split(LIST_SEPARATOR)
        .flat_map(split_joined)
        .filter_map(|role| bounded(&role))
        .filter(|role| role != UNKNOWN_ROLE)
    {
        add_names(credits, role, &names);
    }
}

/// Add `names` to `role`, keeping first-seen order and the bounds of
/// `yt_core`; a role is added only with names.
fn add_names(credits: &mut Vec<CreditRole>, role: String, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let index = match credits.iter().position(|known| known.role == role) {
        Some(index) => index,
        None if credits.len() < MAX_CREDIT_ROLES => {
            credits.push(CreditRole {
                role,
                names: Vec::new(),
            });
            credits.len() - 1
        }
        None => return,
    };
    let entry = &mut credits[index].names;
    for name in names {
        if entry.len() < MAX_CREDIT_NAMES && !entry.contains(name) {
            entry.push(name.clone());
        }
    }
}

/// «Composer Lyricist» becomes «Composer» and «Lyricist»; any other role is
/// kept whole, with the generator's «Co- Producer» and «A& R» spacing fixed.
fn split_joined(role: &str) -> Vec<String> {
    let role = tidy(role);
    let words: Vec<&str> = role.split_whitespace().collect();
    if words.len() > 1 && words.iter().all(|word| JOINED_ROLES.contains(word)) {
        words.into_iter().map(str::to_owned).collect()
    } else {
        vec![role]
    }
}

/// Remove the space the generator inserts after a hyphen or ampersand that
/// directly follows a letter.
fn tidy(role: &str) -> String {
    let mut tidy = String::with_capacity(role.len());
    let mut previous = ' ';
    let mut characters = role.trim().chars().peekable();
    while let Some(character) = characters.next() {
        tidy.push(character);
        if matches!(character, '-' | '&') && previous != ' ' && characters.peek() == Some(&' ') {
            characters.next();
        }
        previous = character;
    }
    tidy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::poison;

    /// The layout of real auto-generated descriptions (2026-11-11), with
    /// placeholder names: a title and an album that contain «: », a quoted
    /// provider, joined and comma-separated roles, the generator's «Co- » and
    /// «A& R» spacing and its «Unknown» role.
    const DESCRIPTION: &str = "Provided to YouTube by 'Fixture Distribution'

Song Title: Part I · Artist One · Guest Two

Album: The Collection, Vol. 1

℗ 2024 Fixture Records, under exclusive license to Example Music

Released on: 2024-06-20

Composer Lyricist, Programmer, Producer: Writer One
Producer: Producer Two
Producer, Mixing Engineer, Mastering Engineer: Engineer  Three
Co- Producer: Producer Four
A& R Director: Manager Five
Associated Performer: Artist One, Guest Two
Unknown: Engineer Three
Lyricist, Composer: Writer Six
Producer: Producer Two

Auto-generated by YouTube.";

    /// The description of the art track whose credits page is recorded in
    /// `music_track_credits.json`, with the same placeholder names: it names
    /// producers and engineers but neither performers nor writers.
    const TRACK_DESCRIPTION: &str = "Provided to YouTube by Fixture Distribution

Song Title · Artist One · Artist Two

Album Title

℗ 2024 Fixture Records

Released on: 2024-06-20

Producer: Producer One
Producer: Producer Two “Nickname”
Producer, Mixing Engineer, Mastering Engineer: Producer Three
Music  Publisher: Fixture Records

Auto-generated by YouTube.";

    fn roles(credits: &Credits) -> Vec<(&str, Vec<&str>)> {
        credits
            .roles
            .iter()
            .map(|role| {
                (
                    role.role.as_str(),
                    role.names.iter().map(String::as_str).collect(),
                )
            })
            .collect()
    }

    fn track_credits_page() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/music_track_credits.json")).unwrap()
    }

    fn sections(page: &mut Value) -> &mut Vec<Value> {
        page.pointer_mut(
            "/onResponseReceivedActions/0/openPopupAction/popup/dismissableDialogRenderer/sections",
        )
        .and_then(Value::as_array_mut)
        .unwrap()
    }

    fn set_text(section: &mut Value, field: &str, text: &str) {
        section[TRACK_CREDITS_SECTION][field] = json!({ "runs": [{ "text": text }] });
    }

    #[test]
    fn auto_generated_descriptions_give_provider_label_date_and_merged_roles() {
        let credits = parse_credits(DESCRIPTION).unwrap();
        assert!(credits.validate().is_ok());
        assert_eq!(credits.provider.as_deref(), Some("Fixture Distribution"));
        assert_eq!(
            credits.label.as_deref(),
            Some("℗ 2024 Fixture Records, under exclusive license to Example Music")
        );
        assert_eq!(credits.released.as_deref(), Some("2024-06-20"));
        assert_eq!(
            roles(&credits),
            [
                ("Composer", vec!["Writer One", "Writer Six"]),
                ("Lyricist", vec!["Writer One", "Writer Six"]),
                ("Programmer", vec!["Writer One"]),
                (
                    "Producer",
                    vec!["Writer One", "Producer Two", "Engineer Three"]
                ),
                ("Mixing Engineer", vec!["Engineer Three"]),
                ("Mastering Engineer", vec!["Engineer Three"]),
                ("Co-Producer", vec!["Producer Four"]),
                ("A&R Director", vec!["Manager Five"]),
                ("Associated Performer", vec!["Artist One", "Guest Two"]),
            ]
        );
        assert_eq!(
            parse_credits(&DESCRIPTION.replace('\n', "\r\n")),
            Some(credits)
        );
    }

    #[test]
    fn other_descriptions_and_missing_or_malformed_lines_give_nothing_invented() {
        assert_eq!(
            parse_credits("Official video of the song.\n\nProducer: Someone"),
            None
        );
        assert_eq!(parse_credits(""), None);
        let minimal = parse_credits(
            "Provided to YouTube by Distributor\n\nSong · Artist\n\nSong\n\nReleased on: 2024-13-01\n\nAuto-generated by YouTube.",
        )
        .unwrap();
        assert_eq!(minimal.provider.as_deref(), Some("Distributor"));
        assert_eq!(
            (minimal.label.as_deref(), minimal.released.as_deref()),
            (None, None)
        );
        assert!(minimal.roles.is_empty());
        for date in [
            "2024",
            "2024-06",
            "24-06-20",
            "2024-06-32",
            "2024-00-10",
            "2024-06-2x",
        ] {
            assert_eq!(release_date(date), None, "{date}");
        }
        assert_eq!(release_date("1999-12-31"), Some("1999-12-31"));
        assert_eq!(unquoted("\"Label\""), "Label");
        assert_eq!(unquoted("'Unbalanced"), "'Unbalanced");
    }

    #[test]
    fn roles_and_names_stay_within_the_domain_bounds() {
        let mut description = String::from("Provided to YouTube by D\n\nT · A\n\nAlbum\n\n");
        for role in 0..MAX_CREDIT_ROLES + 8 {
            description.push_str(&format!("Role {role}: Person\n"));
        }
        let people: Vec<String> = (0..MAX_CREDIT_NAMES + 6)
            .map(|n| format!("Person {n}"))
            .collect();
        description.push_str(&format!("Role 0: {}\n", people.join(", ")));
        description.push_str(&format!(
            "Role 1: {}, Ok\u{7}Name, Real Name\n",
            "x".repeat(MAX_CREDIT_CHARS + 1)
        ));
        let credits = parse_credits(&description).unwrap();
        assert!(credits.validate().is_ok());
        assert_eq!(credits.roles.len(), MAX_CREDIT_ROLES);
        assert_eq!(credits.roles[0].names.len(), MAX_CREDIT_NAMES);
        assert_eq!(credits.roles[1].names, ["Person", "Real Name"]);
        assert_eq!(split_joined("Composer Lyricist"), ["Composer", "Lyricist"]);
        assert_eq!(split_joined("Vocal Producer"), ["Vocal Producer"]);
        assert_eq!(tidy("Production Co- Ordinator"), "Production Co-Ordinator");
        assert_eq!(tidy("Mixing & Mastering"), "Mixing & Mastering");
    }

    #[test]
    fn descriptions_are_read_from_the_watch_page_or_its_description_panel() {
        let primary = json!({"contents": {"twoColumnWatchNextResults": {"results": {"results": {"contents": [
            {"videoPrimaryInfoRenderer": {"title": {"runs": [{"text": "Song"}]}}},
            {"videoSecondaryInfoRenderer": {"attributedDescription": {"content": "Provided to YouTube by A"}}}
        ]}}}}});
        assert_eq!(
            watch_description(&primary),
            Some("Provided to YouTube by A")
        );
        let panel = json!({"engagementPanels": [
            {"engagementPanelSectionListRenderer": {"content": {"structuredDescriptionContentRenderer": {"items": [
                {"videoDescriptionHeaderRenderer": {}},
                {"expandableVideoDescriptionBodyRenderer": {"attributedDescriptionBodyText": {"content": "Provided to YouTube by B"}}}
            ]}}}}
        ]});
        assert_eq!(watch_description(&panel), Some("Provided to YouTube by B"));
        assert_eq!(
            watch_description(&json!({"contents": 42, "engagementPanels": "x"})),
            None
        );
    }

    #[test]
    fn the_credits_page_adds_performers_and_writers_before_the_description_roles() {
        let description = parse_credits(TRACK_DESCRIPTION).unwrap();
        let credits = with_track_credits(description.clone(), &track_credits_page());
        assert!(credits.validate().is_ok());
        assert_eq!(
            (
                credits.provider.as_deref(),
                credits.label.as_deref(),
                credits.released.as_deref()
            ),
            (
                Some("Fixture Distribution"),
                Some("℗ 2024 Fixture Records"),
                Some("2024-06-20")
            )
        );
        assert_eq!(
            roles(&credits),
            [
                ("Performer", vec!["Artist One", "Artist Two"]),
                ("Writer", vec!["Artist Two"]),
                (
                    "Producer",
                    vec!["Producer One", "Producer Two “Nickname”", "Producer Three"]
                ),
                ("Mixing Engineer", vec!["Producer Three"]),
                ("Mastering Engineer", vec!["Producer Three"]),
                ("Music Publisher", vec!["Fixture Records"]),
            ]
        );
        let mut without_sections = track_credits_page();
        sections(&mut without_sections).clear();
        assert_eq!(
            with_track_credits(description.clone(), &without_sections),
            description
        );
    }

    #[test]
    fn page_names_the_description_already_details_are_not_repeated() {
        let description = TRACK_DESCRIPTION.replace(
            "Music  Publisher: Fixture Records",
            "Associated Performer: Artist One\nComposer Lyricist: Artist Two",
        );
        let credits =
            with_track_credits(parse_credits(&description).unwrap(), &track_credits_page());
        assert!(credits.validate().is_ok());
        assert_eq!(
            roles(&credits),
            [
                ("Performer", vec!["Artist Two"]),
                (
                    "Producer",
                    vec!["Producer One", "Producer Two “Nickname”", "Producer Three"]
                ),
                ("Mixing Engineer", vec!["Producer Three"]),
                ("Mastering Engineer", vec!["Producer Three"]),
                ("Associated Performer", vec!["Artist One"]),
                ("Composer", vec!["Artist Two"]),
                ("Lyricist", vec!["Artist Two"]),
            ]
        );
        assert!(!detailed(&credits.roles, "Producer", "Producer One"));
    }

    #[test]
    fn section_titles_of_every_served_language_name_their_roles() {
        // As YouTube served them on 2026-11-11 for es, en, pt, pt-PT, fr,
        // fr-CA, it and de sessions.
        let observed = [
            ["Interpretada por", "Escrita por", "Producida por"],
            ["Performed by", "Written by", "Produced by"],
            ["Interpretação de", "Composição de", "Produzido por"],
            ["Interpretada por", "Composta por", "Produzida por"],
            ["Interprète(s)", "Auteur(s)/Compositeur(s)", "Producteur(s)"],
            ["Interprétée par", "Écrite par", "Produite par"],
            ["Eseguito da", "Scritto da", "Prodotto da"],
            ["Interpretiert von", "Geschrieben von", "Produziert von"],
        ];
        for titles in observed {
            let mut page = track_credits_page();
            for (section, title) in sections(&mut page).iter_mut().zip(titles) {
                set_text(section, "title", title);
            }
            let found: Vec<String> = track_credit_roles(&page)
                .into_iter()
                .map(|role| role.role)
                .collect();
            assert_eq!(found, ["Performer", "Writer", "Producer"], "{titles:?}");
        }
        let skipped = [
            "Metadatos musicales proporcionados por",
            "Metadatos de música proporcionados por",
            "Music metadata provided by",
            "Metadados de música fornecidos por",
            "Metadados de música enviados por",
            "Métadonnées du titre",
            "Métadonnées de la musique fournies par",
            "Metadati dei contenuti musicali forniti da",
            "Musik-Metadaten von",
            "作詞",
            "Interpretada",
            "Escrita por Artist One",
            "",
        ];
        for title in skipped {
            let mut page = track_credits_page();
            for section in sections(&mut page).iter_mut() {
                set_text(section, "title", title);
            }
            assert!(track_credit_roles(&page).is_empty(), "{title}");
        }
    }

    #[test]
    fn credits_page_names_and_merged_roles_stay_within_the_domain_bounds() {
        let mut page = track_credits_page();
        let crowd: Vec<String> = (0..MAX_CREDIT_NAMES + 6)
            .map(|n| format!("Person {n}"))
            .collect();
        let writers = format!(
            "Writer One\n  Writer   One \n\n{}\nBad\u{7}Name\nWriter Two\r\n",
            "x".repeat(MAX_CREDIT_CHARS + 1)
        );
        let found = sections(&mut page);
        set_text(&mut found[0], "subtitle", &crowd.join("\n"));
        set_text(&mut found[1], "subtitle", &writers);
        set_text(&mut found[2], "subtitle", "\n \n");
        let duplicate = found[1].clone();
        found.push(duplicate);
        let roles = track_credit_roles(&page);
        assert_eq!(roles.len(), 2);
        assert_eq!(roles[0].names, crowd[..MAX_CREDIT_NAMES]);
        assert_eq!(roles[1].names, ["Writer One", "Writer Two"]);

        let mut description = String::from("Provided to YouTube by D\n\nT · A\n\nAlbum\n\n");
        for role in 0..MAX_CREDIT_ROLES + 3 {
            description.push_str(&format!("Role {role}: Person\n"));
        }
        let credits = with_track_credits(parse_credits(&description).unwrap(), &page);
        assert!(credits.validate().is_ok());
        assert_eq!(credits.roles.len(), MAX_CREDIT_ROLES);
        assert_eq!(
            (
                credits.roles[0].role.as_str(),
                credits.roles[2].role.as_str()
            ),
            ("Performer", "Role 0")
        );
    }

    #[test]
    fn malformed_credits_pages_never_panic_or_invent_roles() {
        let description = parse_credits(TRACK_DESCRIPTION).unwrap();
        let known = |role: &CreditRole| {
            TRACK_CREDIT_ROLES
                .iter()
                .any(|(name, _)| *name == role.role)
                || description.roles.iter().any(|own| own.role == role.role)
        };
        let keys = [
            "onResponseReceivedActions",
            "openPopupAction",
            "popup",
            TRACK_CREDITS_DIALOG,
            "sections",
            TRACK_CREDITS_SECTION,
            "title",
            "subtitle",
            "runs",
            "text",
        ];
        let garbage = [
            json!(null),
            json!(42),
            json!("Escrita por"),
            json!([[]]),
            json!({"runs": "x"}),
            json!({"runs": [{"text": 7}, {}, null]}),
        ];
        for key in keys {
            for with in &garbage {
                let mut page = track_credits_page();
                poison(&mut page, key, with);
                let credits = with_track_credits(description.clone(), &page);
                assert!(credits.validate().is_ok(), "{key} {with}");
                assert!(credits.roles.iter().all(known), "{key} {with}");
            }
        }
        for page in [
            json!(null),
            json!([]),
            json!("MPTC"),
            json!({"onResponseReceivedActions": {}}),
            json!({"onResponseReceivedActions": [{"openPopupAction": {"popup": {
                TRACK_CREDITS_DIALOG: {"sections": [7, {TRACK_CREDITS_SECTION: "x"}]}
            }}}]}),
        ] {
            assert_eq!(with_track_credits(description.clone(), &page), description);
        }
    }
}
