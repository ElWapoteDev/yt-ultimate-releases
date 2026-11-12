# Favorites

A heart marks any song as a favorite: a library track or online media from YouTube. Favorites have their own page
(«Favoritos»), a Home shelf, and they weigh in recommendations.

## Files

- `crates/yt-core/src/favorites.rs`: `set_favorite`, `set_remote_favorite`, `favorite_ids`, `favorites`.
- `crates/yt-core/src/migrations/0003_listening_and_favorites.sql`: the `rust_favorites` table and the migration of
  earlier library favorites.
- `crates/yt-app/src/favorites.rs`: the controller (`is_favorite`, `toggle_favorite`, `load_favorite_tracks`,
  `favorite_entry`) and the shared action labels.
- `crates/yt-app/src/favorites_ui.rs`, `crates/yt-app/src/favorites_ui.css`: `FavoriteButton` and `FavoritesPage`.
  The stylesheet is included once at the root because the button appears everywhere.
- Call sites: the player bar, `TrackRow` and the library card badge (`crates/yt-app/src/app.rs`), search results
  (`crates/yt-app/src/search_ui.rs`), Home cards (`crates/yt-app/src/home_ui.rs`), the full-screen view
  (`crates/yt-app/src/now_playing.rs`) and the track menus (`crates/yt-app/src/context_menu.rs`).

## Behavior

- `AppState::favorites` holds every favorite id, loaded once at startup, so marking an item never queries the
  database.
- `toggle_favorite` is optimistic: the heart, the library flag and the playing track change at once; the write
  follows and a failure restores the previous state with an error notice. `favorites_revision` reloads the
  Favorites page after a change.
- Online media is remembered (`rust_remote_media`) in the same transaction as its favorite, so the Favorites page and
  Home show it without asking YouTube. Unfavoriting online media nobody else references forgets it.
- Re-adding keeps the original date; the list is newest first. Library favorites whose file is missing are kept in
  the database and left off the page because they cannot play.
- `FavoriteButton` is a `button`; never nest it inside another button (library cards show a non-interactive badge
  and offer the action in their menu).

## Trust boundary

Toggle input is a `Track` the application already holds; online identities are validated by
`RemoteMedia::from_parts` and `yt-core` rejects unknown ids.

## Tests

`yt-core` favorites tests (shared ordering, unknown ids, cleanup on removal, migration of schema 1/2 favorites) and
`crates/yt-app/src/favorites.rs` (optimistic toggle, persistence, rollback, online favorites listed with local ones).
