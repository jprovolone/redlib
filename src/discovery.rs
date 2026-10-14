//! Cookie-local favorites and explicitly curated related-community discovery.
//! No preferences serialization changes and no popular/random fallback.
use crate::server::{RequestExt, ResponseExt};
use crate::utils::{filter_posts, get_filters, redirect, setting, sfw_only, template, Post, Preferences};
use askama::Template;
use cookie::{Cookie, SameSite};
use hyper::{body::HttpBody, Body, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;
use time::{Duration, OffsetDateTime};
use url::form_urlencoded;

const PAGE_SIZE: usize = 25;
const END: &str = "end";
const MAX_BODY: usize = 16 * 1024;

pub struct FeedEntry {
	pub post: Post,
	pub reason: String,
	pub discovery: bool,
}

#[derive(Template)]
#[template(path = "for_you.html")]
pub struct FeedTemplate {
	pub prefs: Preferences,
	pub url: String,
	pub entries: Vec<FeedEntry>,
	pub favorite_count: usize,
	pub discovery_label: String,
	pub notice: String,
	pub next_url: String,
	pub previous_url: String,
}

#[derive(Template)]
#[template(path = "discover_settings.html")]
pub struct SetupTemplate {
	pub prefs: Preferences,
	pub url: String,
	pub favorites: String,
	pub interested: String,
	pub less_interested: String,
	pub excluded: String,
	pub discovery_level: String,
	pub notice: String,
}

#[derive(Deserialize)]
struct RelatedGroup {
	topic: String,
	communities: Vec<String>,
}

static RELATED: LazyLock<Vec<RelatedGroup>> =
	LazyLock::new(|| serde_json::from_str(include_str!("../static/related-communities.json")).expect("embedded related communities must be valid JSON"));

fn normalize_name(value: &str) -> Result<String, String> {
	let value = value.trim();
	let value = value.strip_prefix("/r/").or_else(|| value.strip_prefix("r/")).unwrap_or(value);
	if !(2..=21).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || value.to_ascii_lowercase().starts_with("u_") {
		return Err(format!("Invalid community '{value}': use 2–21 ASCII letters, numbers or underscores (not user feeds)."));
	}
	Ok(value.to_ascii_lowercase())
}

fn tokens(value: &str) -> impl Iterator<Item = &str> {
	value.split(|c: char| c == ',' || c == '+' || c.is_whitespace()).filter(|s| !s.is_empty())
}

fn validate_names(value: &str, max: usize) -> Result<Vec<String>, String> {
	let mut result = Vec::new();
	let mut seen = HashSet::new();
	for token in tokens(value) {
		let name = normalize_name(token)?;
		if seen.insert(name.clone()) {
			result.push(name);
		}
		if result.len() > max {
			return Err(format!("At most {max} communities are allowed."));
		}
	}
	Ok(result)
}

// Existing subscriptions and manually modified cookies are untrusted. Ignore invalid
// names and cap them rather than passing arbitrary paths to the upstream client.
fn cookie_names(value: &str, max: usize) -> Vec<String> {
	let mut seen = HashSet::new();
	tokens(value).filter_map(|s| normalize_name(s).ok()).filter(|s| seen.insert(s.clone())).take(max).collect()
}

fn favorites(req: &Request<Body>) -> Vec<String> {
	match req.cookie("fy_favorites") {
		Some(cookie) => cookie_names(cookie.value(), 50),
		None => cookie_names(&setting(req, "subscriptions"), 50),
	}
}

/// Cookie-local post ratings, independent of subscriptions and favorites.
pub(crate) fn rating_lists(req: &Request<Body>) -> (Vec<String>, Vec<String>, Vec<String>) {
	let read = |name, max| req.cookie(name).map(|c| cookie_names(c.value(), max)).unwrap_or_default();
	(read("fy_interested", 50), read("fy_less_interested", 100), read("fy_excluded", 100))
}

fn seed_union(req: &Request<Body>) -> Vec<String> {
	let mut seeds = favorites(req);
	for name in rating_lists(req).0 {
		if !seeds.contains(&name) {
			seeds.push(name);
		}
	}
	seeds
}

fn exclusions(req: &Request<Body>) -> HashSet<String> {
	let mut filters: HashSet<String> = get_filters(req).into_iter().map(|s| s.to_ascii_lowercase()).collect();
	if let Some(cookie) = req.cookie("fy_excluded") {
		filters.extend(cookie_names(cookie.value(), 100));
	}
	filters
}

fn level(req: &Request<Body>) -> String {
	match req.cookie("fy_level").map(|c| c.value().to_string()).as_deref() {
		Some("low") => "low",
		Some("high") => "high",
		_ => "balanced",
	}
	.to_string()
}

fn percentage(level: &str) -> usize {
	match level {
		"low" => 20,
		"high" => 50,
		_ => 30,
	}
}

/// Number of unlisted favorites looked up per feed load (results cached for a day).
const SEARCHED_SEEDS: usize = 8;
/// Suggestions taken from each favorite's search results.
const PER_SEED_RESULTS: usize = 3;
/// Skip tiny or abandoned communities.
const MIN_SUBSCRIBERS: f64 = 10_000.0;

fn in_catalog(seed: &str) -> bool {
	RELATED.iter().any(|g| g.communities.iter().any(|c| c == seed))
}

/// Search terms for a community name: underscores become spaces.
fn search_terms(seed: &str) -> String {
	seed.replace('_', " ")
}

/// Picks suggestions from a Reddit community-search response: valid, SFW,
/// established communities other than the seed, in Reddit's relevance order.
fn parse_search_results(value: &serde_json::Value, seed: &str, limit: usize) -> Vec<String> {
	let mut seen = HashSet::new();
	value["data"]["children"]
		.as_array()
		.map(|children| {
			children
				.iter()
				.filter(|c| !c["data"]["over18"].as_bool().unwrap_or(true))
				.filter(|c| c["data"]["subscribers"].as_f64().unwrap_or_default() >= MIN_SUBSCRIBERS)
				.filter(|c| c["data"]["subreddit_type"].as_str().unwrap_or("public") == "public")
				.filter_map(|c| normalize_name(c["data"]["display_name"].as_str().unwrap_or_default()).ok())
				.filter(|name| name != seed && seen.insert(name.clone()))
				.take(limit)
				.collect()
		})
		.unwrap_or_default()
}

/// Communities similar to a favorite that isn't in the curated list, found via
/// Reddit's community search. Cached for a day so pagination stays stable and
/// repeat visits don't re-query Reddit.
#[cached::proc_macro::cached(size = 1024, time = 86400, result = true)]
async fn searched_related(seed: String) -> Result<Vec<String>, String> {
	// Built in its own scope: the serializer isn't `Send` and mustn't live across the await.
	let path = {
		let mut query = form_urlencoded::Serializer::new(String::new());
		query
			.append_pair("q", &search_terms(&seed))
			.append_pair("limit", "10")
			.append_pair("include_over_18", "off");
		format!("/subreddits/search.json?{}", query.finish())
	};
	let value = crate::client::json(path, false).await?;
	Ok(parse_search_results(&value, &seed, PER_SEED_RESULTS))
}

/// Curated candidates first, then search-based ones for unlisted favorites.
/// Returns the candidates and whether any lookup failed.
async fn related_candidates(seeds: &[String], excluded: &HashSet<String>) -> (Vec<(String, String)>, bool) {
	let mut result = candidates(seeds, excluded);
	let unlisted: Vec<String> = seeds.iter().filter(|s| !in_catalog(s)).take(SEARCHED_SEEDS).cloned().collect();
	if unlisted.is_empty() || result.len() >= 24 {
		return (result, false);
	}
	let mut lookups = tokio::task::JoinSet::new();
	for (index, seed) in unlisted.into_iter().enumerate() {
		lookups.spawn(async move { (index, seed.clone(), searched_related(seed).await) });
	}
	let mut found = Vec::new();
	let mut failed = false;
	while let Some(joined) = lookups.join_next().await {
		match joined {
			Ok((index, seed, Ok(names))) => found.push((index, seed, names)),
			_ => failed = true,
		}
	}
	// Restore favorite order so the candidate list (and its pagination) is stable.
	found.sort_by_key(|(index, _, _)| *index);
	let seeds_set: HashSet<&String> = seeds.iter().collect();
	let mut seen: HashSet<String> = result.iter().map(|(name, _)| name.clone()).collect();
	// Round-robin across favorites so one favorite doesn't take every slot.
	for round in 0..PER_SEED_RESULTS {
		for (_, seed, names) in &found {
			if let Some(name) = names.get(round) {
				if result.len() < 24 && !seeds_set.contains(name) && !excluded.contains(name) && seen.insert(name.clone()) {
					result.push((name.clone(), seed.clone()));
				}
			}
		}
	}
	(result, failed)
}

/// Candidate order and explanations depend only on seed order and the embedded
/// groups, never on hash iteration order, browsing history, or unrelated popularity.
fn candidates(seeds: &[String], excluded: &HashSet<String>) -> Vec<(String, String)> {
	let seeds_set: HashSet<&String> = seeds.iter().collect();
	let mut seen = HashSet::new();
	let mut result = Vec::new();
	for seed in seeds {
		for group in RELATED.iter().filter(|g| !g.topic.is_empty() && g.communities.contains(seed)) {
			for community in &group.communities {
				if !seeds_set.contains(community) && !excluded.contains(community) && seen.insert(community.clone()) {
					result.push((community.clone(), seed.clone()));
					if result.len() == 24 {
						return result;
					}
				}
			}
		}
	}
	result
}

fn query(req: &Request<Body>) -> HashMap<String, String> {
	form_urlencoded::parse(req.uri().query().unwrap_or_default().as_bytes()).into_owned().collect()
}

fn saved_notice(query: &HashMap<String, String>) -> String {
	match query.get("saved").map(String::as_str) {
		Some("1") => "For You saved.",
		Some("hidden") => "Excluded. That community won't appear in For You.",
		_ => "",
	}
	.to_string()
}

fn setup(req: &Request<Body>, notice: String) -> SetupTemplate {
	SetupTemplate {
		prefs: Preferences::new(req),
		url: req.uri().to_string(),
		favorites: favorites(req).join(", "),
		interested: rating_lists(req).0.join(", "),
		less_interested: rating_lists(req).1.join(", "),
		excluded: req.cookie("fy_excluded").map(|c| cookie_names(c.value(), 100).join(", ")).unwrap_or_default(),
		discovery_level: level(req),
		notice,
	}
}

pub async fn settings(req: Request<Body>) -> Result<Response<Body>, String> {
	if req.method() != Method::GET {
		return Ok(method_not_allowed("GET"));
	}
	Ok(template(&setup(&req, saved_notice(&query(&req)))))
}

fn method_not_allowed(allow: &str) -> Response<Body> {
	Response::builder()
		.status(StatusCode::METHOD_NOT_ALLOWED)
		.header("allow", allow)
		.body(Body::empty())
		.unwrap()
}

fn bad_setup(mut setup: SetupTemplate, notice: String) -> Response<Body> {
	setup.notice = notice;
	let mut response = template(&setup);
	*response.status_mut() = StatusCode::BAD_REQUEST;
	response
}

async fn read_form(req: &mut Request<Body>) -> Result<HashMap<String, String>, String> {
	if req
		.headers()
		.get("content-length")
		.and_then(|h| h.to_str().ok())
		.and_then(|s| s.parse::<usize>().ok())
		.is_some_and(|n| n > MAX_BODY)
	{
		return Err("Form is too large (maximum 16 KiB).".to_string());
	}
	// Bound allocation even when Content-Length is absent or untrusted.
	let mut bytes = Vec::new();
	while let Some(chunk) = req.body_mut().data().await {
		let chunk = chunk.map_err(|e| format!("Unable to read form: {e}"))?;
		if bytes.len() + chunk.len() > MAX_BODY {
			return Err("Form is too large (maximum 16 KiB).".to_string());
		}
		bytes.extend_from_slice(&chunk);
	}
	Ok(form_urlencoded::parse(&bytes).into_owned().collect())
}

fn insert_cookie(response: &mut Response<Body>, name: &'static str, value: String) {
	response.insert_cookie(
		Cookie::build((name, value))
			.path("/")
			.http_only(true)
			.same_site(SameSite::Lax)
			.expires(OffsetDateTime::now_utc() + Duration::days(365))
			.into(),
	);
}

/// Keeps a saved For You favorites list in step with subscribing/unsubscribing.
/// If no list was ever saved, For You already follows subscriptions, so nothing
/// is written. User feeds (u_*) aren't supported in For You and are ignored.
pub fn sync_favorites(req: &Request<Body>, response: &mut Response<Body>, communities: &[String], subscribed: bool) {
	let Some(cookie) = req.cookie("fy_favorites") else { return };
	let mut favorites = cookie_names(cookie.value(), 50);
	let before = favorites.clone();
	for community in communities {
		let Ok(name) = normalize_name(community) else { continue };
		if subscribed {
			if !favorites.contains(&name) && favorites.len() < 50 {
				favorites.push(name);
			}
		} else {
			favorites.retain(|f| f != &name);
		}
	}
	if favorites != before {
		insert_cookie(response, "fy_favorites", favorites.join("+"));
	}
}

/// For You settings carried by settings export/import and restore links.
/// Kept separate from `Preferences` so existing export codes keep working:
/// new codes append this after the unchanged `Preferences` bytes.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct ForYouExport {
	/// `None` means "follow subscriptions" (no saved favorites list).
	pub favorites: Option<String>,
	pub excluded: String,
	/// Empty when never chosen (defaults to balanced).
	pub level: String,
}

impl ForYouExport {
	pub fn from_request(req: &Request<Body>) -> Self {
		Self {
			favorites: req.cookie("fy_favorites").map(|c| cookie_names(c.value(), 50).join("+")),
			excluded: req.cookie("fy_excluded").map(|c| cookie_names(c.value(), 100).join("+")).unwrap_or_default(),
			level: if req.cookie("fy_level").is_some() { level(req) } else { String::new() },
		}
	}

	/// Query-string form (no leading separator), empty when nothing is saved.
	pub fn to_query(&self) -> String {
		let mut query = form_urlencoded::Serializer::new(String::new());
		if let Some(favorites) = &self.favorites {
			query.append_pair("fy_favorites", favorites);
		}
		if !self.excluded.is_empty() {
			query.append_pair("fy_excluded", &self.excluded);
		}
		if !self.level.is_empty() {
			query.append_pair("fy_level", &self.level);
		}
		query.finish()
	}

	/// Reads restore-link parameters. Untrusted input is normalized the same way
	/// as cookies: invalid names are dropped, lists are capped, levels validated.
	pub fn from_query(form: &HashMap<Cow<'_, str>, Cow<'_, str>>) -> Self {
		Self {
			favorites: form.get("fy_favorites").map(|v| cookie_names(v, 50).join("+")),
			excluded: form.get("fy_excluded").map(|v| cookie_names(v, 100).join("+")).unwrap_or_default(),
			level: match form.get("fy_level").map(|v| v.as_ref()) {
				Some(level @ ("low" | "balanced" | "high")) => level.to_string(),
				_ => String::new(),
			},
		}
	}

	/// Writes these settings as cookies. With `remove_missing` (full restore),
	/// anything not present is cleared so the restored state matches the export.
	pub fn apply(&self, response: &mut Response<Body>, remove_missing: bool) {
		match &self.favorites {
			Some(favorites) => insert_cookie(response, "fy_favorites", favorites.clone()),
			None if remove_missing => response.remove_cookie("fy_favorites".to_string()),
			None => {}
		}
		if !self.excluded.is_empty() {
			insert_cookie(response, "fy_excluded", self.excluded.clone());
		} else if remove_missing {
			response.remove_cookie("fy_excluded".to_string());
		}
		if !self.level.is_empty() {
			insert_cookie(response, "fy_level", self.level.clone());
		} else if remove_missing {
			response.remove_cookie("fy_level".to_string());
		}
	}
}

/// Ratings are appended separately to settings exports; do not change the legacy
/// `ForYouExport` layout (old binary pairs must remain decodable).
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct ForYouRatings {
	pub interested: String,
	pub less_interested: String,
}

impl ForYouRatings {
	pub fn from_request(req: &Request<Body>) -> Self {
		let (interested, less_interested, _) = rating_lists(req);
		Self {
			interested: interested.join("+"),
			less_interested: less_interested.join("+"),
		}
	}

	pub fn to_query(&self) -> String {
		let mut query = form_urlencoded::Serializer::new(String::new());
		for (name, value, max) in [("fy_interested", &self.interested, 50), ("fy_less_interested", &self.less_interested, 100)] {
			let value = cookie_names(value, max).join("+");
			if !value.is_empty() {
				query.append_pair(name, &value);
			}
		}
		query.finish()
	}

	pub fn from_query(form: &HashMap<Cow<'_, str>, Cow<'_, str>>) -> Self {
		Self {
			interested: form.get("fy_interested").map(|v| cookie_names(v, 50).join("+")).unwrap_or_default(),
			less_interested: form.get("fy_less_interested").map(|v| cookie_names(v, 100).join("+")).unwrap_or_default(),
		}
	}

	pub fn apply(&self, response: &mut Response<Body>, remove_missing: bool) {
		for (name, value, max) in [("fy_interested", &self.interested, 50), ("fy_less_interested", &self.less_interested, 100)] {
			let value = cookie_names(value, max).join("+");
			if !value.is_empty() {
				insert_cookie(response, name, value);
			} else if remove_missing {
				response.remove_cookie(name.to_string());
			}
		}
	}
}

/// Allow old clients without metadata, but never explicit cross-site mutations.
/// Modern browsers send `Sec-Fetch-Site`, which is trusted when present: it is
/// unaffected by reverse proxies rewriting `Host`. Otherwise compare `Origin`
/// with `Host` or `X-Forwarded-Host`, using the Origin scheme for default ports.
fn mutation_forbidden(req: &Request<Body>) -> bool {
	let mut fetch_sites = req.headers().get_all("sec-fetch-site").iter().peekable();
	if fetch_sites.peek().is_some() {
		return fetch_sites.any(|v| v.to_str().map_or(true, |s| s.eq_ignore_ascii_case("cross-site")))
			|| req.headers().get_all("origin").iter().count() > 1;
	}
	let mut origins = req.headers().get_all("origin").iter();
	let Some(origin) = origins.next() else { return false };
	if origins.next().is_some() {
		return true;
	}
	let valid = || {
		let raw_origin = origin.to_str().ok()?;
		if raw_origin.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
			return None;
		}
		let origin = url::Url::parse(raw_origin).ok()?;
		let authority = raw_origin.strip_prefix(&format!("{}://", origin.scheme()))?;
		// Origin is an authority, not an arbitrary URL that URL parsing repairs.
		if authority.trim_end_matches('/').contains('/') || authority.is_empty() {
			return None;
		}
		if !matches!(origin.scheme(), "http" | "https")
			|| !origin.username().is_empty()
			|| origin.password().is_some()
			|| origin.path() != "/"
			|| origin.query().is_some()
			|| origin.fragment().is_some()
		{
			return None;
		}
		let matches = |host: &str| {
			let host = host.trim();
			if host.is_empty() || host.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '?' | '#')) {
				return false;
			}
			url::Url::parse(&format!("{}://{host}/", origin.scheme())).is_ok_and(|host| {
				origin.host_str().is_some() && origin.host_str() == host.host_str() && origin.port_or_known_default() == host.port_or_known_default()
			})
		};
		let mut hosts = req.headers().get_all("host").iter();
		let host_ok = match (hosts.next(), hosts.next()) {
			(Some(host), None) => host.to_str().is_ok_and(matches),
			_ => false,
		};
		let forwarded_ok = req
			.headers()
			.get("x-forwarded-host")
			.and_then(|h| h.to_str().ok())
			.and_then(|h| h.split(',').next())
			.is_some_and(matches);
		Some(host_ok || forwarded_ok)
	};
	valid() != Some(true)
}

fn forbidden() -> Response<Body> {
	Response::builder()
		.status(StatusCode::FORBIDDEN)
		.body(Body::from("Cross-site cookie mutation rejected."))
		.unwrap()
}

fn safe_return_to(value: &str) -> &str {
	if value.len() > 2048
		|| !value.starts_with('/')
		|| value.starts_with("//")
		|| value.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\\' | '"' | '\'' | '<' | '>'))
		|| hyper::header::HeaderValue::from_str(value).is_err()
	{
		return "/for-you";
	}
	// URL parsing must not turn a supposedly relative path into an authority.
	let base = url::Url::parse("https://same-site.invalid/").unwrap();
	match base.join(value) {
		Ok(url) if url.origin() == base.origin() => value,
		_ => "/for-you",
	}
}

const RATING_COOKIES: [(&str, usize); 3] = [("fy_interested", 50), ("fy_less_interested", 100), ("fy_excluded", 100)];

fn strict_ratings(req: &Request<Body>) -> Result<[Vec<String>; 3], String> {
	let read = |(name, max)| validate_names(req.cookie(name).as_ref().map(|c| c.value()).unwrap_or_default(), max);
	Ok([read(RATING_COOKIES[0])?, read(RATING_COOKIES[1])?, read(RATING_COOKIES[2])?])
}

fn choose_rating(req: &Request<Body>, name: &str, rating: &str) -> Result<[Vec<String>; 3], String> {
	let selected = match rating {
		"interested" => Some(0),
		"less" => Some(1),
		"exclude" => Some(2),
		"clear" => None,
		_ => return Err("Rating must be interested, less, exclude, or clear.".to_string()),
	};
	let mut lists = strict_ratings(req)?;
	for list in &mut lists {
		list.retain(|entry| entry != name);
	}
	if let Some(index) = selected {
		lists[index].push(name.to_string());
		if lists[index].len() > RATING_COOKIES[index].1 {
			return Err(format!("At most {} communities are allowed.", RATING_COOKIES[index].1));
		}
	}
	Ok(lists)
}

fn write_ratings(response: &mut Response<Body>, lists: &[Vec<String>; 3]) {
	for ((name, _), list) in RATING_COOKIES.iter().zip(lists) {
		insert_cookie(response, name, list.join("+"));
	}
}

pub async fn rate(mut req: Request<Body>) -> Result<Response<Body>, String> {
	if req.method() != Method::POST {
		return Ok(method_not_allowed("POST"));
	}
	if mutation_forbidden(&req) {
		return Ok(forbidden());
	}
	let mutation = async {
		let form = read_form(&mut req).await?;
		let name = normalize_name(form.get("community").map(String::as_str).unwrap_or_default())?;
		let lists = choose_rating(&req, &name, form.get("rating").map(String::as_str).unwrap_or_default())?;
		let mut response = redirect(safe_return_to(form.get("return_to").map(String::as_str).unwrap_or_default()));
		*response.status_mut() = StatusCode::SEE_OTHER;
		write_ratings(&mut response, &lists);
		Ok::<_, String>(response)
	}
	.await;
	Ok(mutation.unwrap_or_else(|error| Response::builder().status(StatusCode::BAD_REQUEST).body(Body::from(error)).unwrap()))
}

pub async fn configure(mut req: Request<Body>) -> Result<Response<Body>, String> {
	if req.method() != Method::POST {
		return Ok(method_not_allowed("POST"));
	}
	if mutation_forbidden(&req) {
		return Ok(forbidden());
	}
	let mut view = setup(&req, String::new());
	let form = match read_form(&mut req).await {
		Ok(form) => form,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	view.favorites = form.get("favorites").cloned().unwrap_or_default();
	view.excluded = form.get("excluded").cloned().unwrap_or_default();
	// Cached old forms preserve rating cookies, including strict validation.
	view.interested = form
		.get("interested")
		.cloned()
		.unwrap_or_else(|| req.cookie("fy_interested").map(|c| c.value().to_string()).unwrap_or_default());
	view.less_interested = form
		.get("less_interested")
		.cloned()
		.unwrap_or_else(|| req.cookie("fy_less_interested").map(|c| c.value().to_string()).unwrap_or_default());
	view.discovery_level = form.get("discovery_level").cloned().unwrap_or_else(|| "balanced".to_string());
	let names = match validate_names(&view.favorites, 50) {
		Ok(names) => names,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let excluded = match validate_names(&view.excluded, 100) {
		Ok(names) => names,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let mut interested = match validate_names(&view.interested, 50) {
		Ok(names) => names,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let mut less_interested = match validate_names(&view.less_interested, 100) {
		Ok(names) => names,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	less_interested.retain(|name| !excluded.contains(name));
	interested.retain(|name| !excluded.contains(name) && !less_interested.contains(name));
	if !matches!(view.discovery_level.as_str(), "low" | "balanced" | "high") {
		return Ok(bad_setup(view, "Discovery level must be low, balanced, or high.".to_string()));
	}
	let mut response = redirect("/for-you?saved=1");
	insert_cookie(&mut response, "fy_favorites", names.join("+"));
	insert_cookie(&mut response, "fy_excluded", excluded.join("+"));
	insert_cookie(&mut response, "fy_level", view.discovery_level);
	let before = rating_lists(&req);
	if form.contains_key("interested") || interested != before.0 {
		insert_cookie(&mut response, "fy_interested", interested.join("+"));
	}
	if form.contains_key("less_interested") || less_interested != before.1 {
		insert_cookie(&mut response, "fy_less_interested", less_interested.join("+"));
	}
	Ok(response)
}

pub async fn hide(mut req: Request<Body>) -> Result<Response<Body>, String> {
	if req.method() != Method::POST {
		return Ok(method_not_allowed("POST"));
	}
	if mutation_forbidden(&req) {
		return Ok(forbidden());
	}
	let view = setup(&req, String::new());
	let form = match read_form(&mut req).await {
		Ok(form) => form,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let name = match normalize_name(form.get("community").map(String::as_str).unwrap_or_default()) {
		Ok(name) => name,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let lists = match choose_rating(&req, &name, "exclude") {
		Ok(lists) => lists,
		Err(error) => return Ok(bad_setup(view, error)),
	};
	let mut response = redirect("/for-you?saved=hidden");
	write_ratings(&mut response, &lists);
	Ok(response)
}

fn valid_cursor(cursor: &str) -> bool {
	cursor.is_empty()
		|| cursor == END
		|| cursor
			.strip_prefix("t3_")
			.is_some_and(|id| !id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn page_url(sort: &str, favorite_after: &str, discovery_after: &str) -> String {
	let mut query = form_urlencoded::Serializer::new(String::new());
	query
		.append_pair("sort", sort)
		.append_pair("f_after", favorite_after)
		.append_pair("d_after", discovery_after);
	if sort == "top" {
		query.append_pair("t", "day");
	}
	format!("/for-you?{}", query.finish())
}

fn listing_path(names: &[String], sort: &str, after: &str, limit: usize) -> String {
	let mut query = form_urlencoded::Serializer::new(String::new());
	query.append_pair("raw_json", "1").append_pair("limit", &limit.to_string());
	if !after.is_empty() {
		query.append_pair("after", after);
	}
	if sort == "top" {
		query.append_pair("t", "day");
	}
	format!("/r/{}/{}.json?{}", names.join("%2B"), sort, query.finish())
}

async fn listing(names: &[String], sort: &str, after: &str, limit: usize) -> Result<(Vec<Post>, String), String> {
	if names.is_empty() || after == END || limit == 0 {
		return Ok((Vec::new(), END.to_string()));
	}
	let (mut posts, server_after) = Post::fetch(&listing_path(names, sort, after, limit), false).await?;
	// Normally Reddit respects limit. If it does not, advance only past the raw
	// posts actually consumed on this page, including subsequently filtered ones.
	let next = if posts.len() > limit {
		posts.truncate(limit);
		format!("t3_{}", posts.last().unwrap().id)
	} else if server_after.is_empty() {
		END.to_string()
	} else {
		server_after
	};
	Ok((posts, next))
}

fn clean_posts(posts: &mut Vec<Post>, filters: &HashSet<String>, show_nsfw: bool, allowed: &HashSet<String>) {
	// Reuse the normal user/community filter, expanding exact spellings because
	// its current implementation compares case-sensitively.
	let mut exact = filters.clone();
	for post in posts.iter() {
		if filters.contains(&post.community.to_ascii_lowercase()) {
			exact.insert(post.community.clone());
		}
		if filters.contains(&format!("u_{}", post.author.name.to_ascii_lowercase())) {
			exact.insert(format!("u_{}", post.author.name));
		}
	}
	filter_posts(posts, &exact);
	posts.retain(|p| allowed.contains(&p.community.to_ascii_lowercase()) && (show_nsfw || !(p.flags.nsfw || p.nsfw)));
}

/// Stable, soft reduction: invalid IDs remain visible rather than becoming an
/// accidental exclusion. Run after cleaning, without changing raw page cursors.
fn thin_less_interested(posts: &mut Vec<Post>, less: &HashSet<String>) {
	posts.retain(|post| {
		!less.contains(&post.community.to_ascii_lowercase())
			|| post.id.is_empty()
			|| !post.id.bytes().all(|b| b.is_ascii_alphanumeric())
			|| u64::from_str_radix(&post.id, 36).map_or(true, |id| id % 4 == 0)
	});
}

/// Weighted stable interleaving, global ID deduplication, and a soft community
/// cap of five. Over-cap posts are deferred, then used to fill the page: a small
/// seed set must not produce permanently empty pages or skip fetched surplus.
pub fn blend<T>(favorites: Vec<T>, discovery: Vec<T>, discovery_percent: usize, id: impl Fn(&T) -> &str, community: impl Fn(&T) -> &str) -> Vec<T> {
	blend_up_to(favorites, discovery, discovery_percent, id, community, PAGE_SIZE)
}

/// `blend` with a custom page size, for pages that carry extra fresh posts
/// (truncating them would skip posts the cursors have already moved past).
fn blend_up_to<T>(
	favorites: Vec<T>,
	discovery: Vec<T>,
	discovery_percent: usize,
	id: impl Fn(&T) -> &str,
	community: impl Fn(&T) -> &str,
	page_size: usize,
) -> Vec<T> {
	let mut favorites: VecDeque<T> = favorites.into();
	let mut discovery: VecDeque<T> = discovery.into();
	let mut seen = HashSet::new();
	let mut counts = HashMap::new();
	let mut result = Vec::new();
	let mut deferred = Vec::new();
	let mut weight = 0;
	while !favorites.is_empty() || !discovery.is_empty() {
		weight += discovery_percent.min(100);
		let use_discovery = weight >= 100;
		if use_discovery {
			weight -= 100;
		}
		let item = if (use_discovery && !discovery.is_empty()) || favorites.is_empty() {
			discovery.pop_front()
		} else {
			favorites.pop_front()
		};
		let Some(item) = item else { break };
		if !seen.insert(id(&item).to_string()) {
			continue;
		}
		let count = counts.entry(community(&item).to_ascii_lowercase()).or_insert(0);
		if *count < 5 {
			*count += 1;
			result.push(item);
		} else {
			deferred.push(item);
		}
	}
	result.extend(deferred);
	result.truncate(page_size);
	result
}

pub async fn feed(req: Request<Body>) -> Result<Response<Body>, String> {
	if req.method() != Method::GET {
		return Ok(method_not_allowed("GET"));
	}
	let query = query(&req);
	let level = level(&req);
	let percent = percentage(&level);
	let seeds = seed_union(&req);
	let favorite_names: HashSet<String> = favorites(&req).into_iter().collect();
	let less: HashSet<String> = rating_lists(&req).1.into_iter().collect();
	let filters = exclusions(&req);
	let active: Vec<String> = seeds.iter().filter(|s| !filters.contains(*s)).cloned().collect();
	let (related, lookup_failed) = related_candidates(&active, &filters).await;
	let discovery_names: Vec<String> = related.iter().map(|(name, _)| name.clone()).collect();
	let mut notices = vec![saved_notice(&query)];
	if seeds.is_empty() {
		// The empty state explains what to do; no extra notice needed.
	} else if active.is_empty() {
		notices.push("All your favorites and interests are excluded or filtered. Edit For You to change them.".to_string());
	} else if related.is_empty() && lookup_failed {
		notices.push("Couldn't find related communities right now, so you're only seeing your favorites and interests. Refresh to try again.".to_string());
	} else if related.is_empty() {
		notices.push("No related communities found for your favorites and interests, so you're only seeing those communities.".to_string());
	}
	let sort = match query.get("sort").map(String::as_str) {
		Some("new") => "new",
		Some("top") => "top",
		_ => "hot",
	};
	let f_after = query.get("f_after").map(String::as_str).unwrap_or_default();
	let d_after = query.get("d_after").map(String::as_str).unwrap_or_default();
	let mut view = FeedTemplate {
		prefs: Preferences::new(&req),
		url: req.uri().to_string(),
		entries: Vec::new(),
		favorite_count: seeds.len(),
		discovery_label: format!("About {percent}% of posts come from related communities"),
		notice: String::new(),
		next_url: String::new(),
		previous_url: if !f_after.is_empty() || !d_after.is_empty() {
			"/for-you".to_string()
		} else {
			String::new()
		},
	};
	if !valid_cursor(f_after) || !valid_cursor(d_after) {
		view.notice = "Invalid pagination cursor. Return to the first page.".to_string();
		let mut response = template(&view);
		*response.status_mut() = StatusCode::BAD_REQUEST;
		return Ok(response);
	}
	let favorite_available = !active.is_empty() && f_after != END;
	let discovery_available = !discovery_names.is_empty() && d_after != END;
	let discovery_limit = if !discovery_available {
		0
	} else if !favorite_available {
		PAGE_SIZE
	} else {
		PAGE_SIZE * percent / 100
	};
	let favorite_limit = if favorite_available { PAGE_SIZE - discovery_limit } else { 0 };
	// Hot also weaves rising/new posts from favorites and interests into each
	// page, so a refresh brings new posts like the Reddit app does.
	let fresh_cursors = crate::fresh::Cursors::from_query(req.uri().query().unwrap_or_default(), "fr_after", "fn_after");
	let fresh = sort == "hot" && !active.is_empty();
	let fresh_feed = active.join("%2B");
	let allowed_active: HashSet<String> = active.iter().cloned().collect();
	let (favorite_result, discovery_result, (mut rising, mut new, fresh_next)) = tokio::join!(
		listing(&active, sort, f_after, favorite_limit),
		listing(&discovery_names, sort, d_after, discovery_limit),
		async {
			if fresh && !fresh_cursors.exhausted() {
				crate::fresh::extras(&fresh_feed, &fresh_cursors, false, "").await
			} else {
				let done = crate::fresh::Cursors {
					rising: END.into(),
					new: END.into(),
				};
				(Vec::new(), Vec::new(), done)
			}
		}
	);
	let (mut favorite_posts, favorite_next) = match favorite_result {
		Ok(result) => result,
		Err(error) => {
			notices.push(format!("Couldn't load posts from your favorites and interests ({error}). Refresh to try again."));
			(Vec::new(), f_after.to_string())
		}
	};
	let (mut discovery_posts, discovery_next) = match discovery_result {
		Ok(result) => result,
		Err(error) => {
			notices.push(format!("Couldn't load suggested posts ({error}). Refresh to try again."));
			(Vec::new(), d_after.to_string())
		}
	};
	let show_nsfw = view.prefs.show_nsfw == "on" && !sfw_only();
	clean_posts(&mut favorite_posts, &filters, show_nsfw, &allowed_active);
	clean_posts(&mut rising, &filters, show_nsfw, &allowed_active);
	clean_posts(&mut new, &filters, show_nsfw, &allowed_active);
	thin_less_interested(&mut rising, &less);
	thin_less_interested(&mut new, &less);
	let extra_count = rising.len() + new.len();
	let mut favorite_posts = crate::fresh::weave(favorite_posts, rising, new, |p| &p.id, |p| p.flags.stickied, &mut fastrand::Rng::new());
	clean_posts(&mut discovery_posts, &filters, show_nsfw, &discovery_names.into_iter().collect());
	thin_less_interested(&mut favorite_posts, &less);
	thin_less_interested(&mut discovery_posts, &less);
	let favorite_entries: Vec<FeedEntry> = favorite_posts
		.into_iter()
		.map(|post| FeedEntry {
			reason: if favorite_names.contains(&post.community.to_ascii_lowercase()) {
				"From your favorites"
			} else {
				"From your interests"
			}
			.to_string(),
			post,
			discovery: false,
		})
		.collect();
	let reasons: HashMap<String, String> = related.into_iter().collect();
	let discovery_entries: Vec<FeedEntry> = discovery_posts
		.into_iter()
		.filter_map(|post| {
			let seed = reasons.get(&post.community.to_ascii_lowercase())?;
			Some(FeedEntry {
				reason: format!("Suggested because you like r/{seed}"),
				post,
				discovery: true,
			})
		})
		.collect();
	view.entries = blend_up_to(favorite_entries, discovery_entries, percent, |e| &e.post.id, |e| &e.post.community, PAGE_SIZE + extra_count);
	let fresh_more = fresh && !fresh_next.exhausted();
	if favorite_next != END || discovery_next != END || fresh_more {
		view.next_url = page_url(sort, &favorite_next, &discovery_next);
		if fresh_more {
			view.next_url.push_str(&fresh_next.to_query("fr_after", "fn_after"));
		}
	}
	view.notice = notices.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
	Ok(template(&view))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn embedded_groups_are_valid_unique_and_have_no_self_suggestions() {
		let mut topics = HashSet::new();
		let mut communities = HashSet::new();
		assert_eq!(RELATED.len(), 15);
		for group in RELATED.iter() {
			assert!(topics.insert(&group.topic));
			assert!(group.communities.len() >= 2);
			for seed in &group.communities {
				assert_eq!(normalize_name(seed).unwrap(), *seed);
				assert!(communities.insert(seed));
				let related = candidates(std::slice::from_ref(seed), &HashSet::new());
				assert!(!related.iter().any(|(name, _)| name == seed));
				assert!(related.iter().all(|(_, reason)| reason == seed));
			}
		}
	}

	#[test]
	fn deterministic_candidates_honor_favorites_exclusions_and_cap() {
		let seeds = vec!["linux".to_string(), "linuxquestions".to_string(), "space".to_string()];
		let excluded = HashSet::from(["linux4noobs".to_string()]);
		let expected = vec![
			("opensource".to_string(), "linux".to_string()),
			("astronomy".to_string(), "space".to_string()),
			("astrophotography".to_string(), "space".to_string()),
			("nasa".to_string(), "space".to_string()),
		];
		assert_eq!(candidates(&seeds, &excluded), expected);
		assert_eq!(candidates(&seeds, &excluded), candidates(&seeds, &excluded));
		assert!(candidates(&["unknowncommunity".to_string()], &HashSet::new()).is_empty());
		let seeds: Vec<String> = RELATED.iter().map(|g| g.communities[0].clone()).collect();
		assert_eq!(candidates(&seeds, &HashSet::new()).len(), 24);
	}

	#[test]
	fn validates_names_counts_and_user_feed_boundary() {
		assert_eq!(validate_names("r/Linux, /r/Space +linux\nbooks", 50).unwrap(), vec!["linux", "space", "books"]);
		for invalid in ["x", "u_example", "space/foo", "abc?sort=new", "éé", "abcdefghijklmnopqrstuv", "<script>"] {
			assert!(validate_names(invalid, 50).is_err(), "{invalid}");
		}
		assert!(validate_names("linux,space", 1).is_err());
		assert!(validate_names("", 50).unwrap().is_empty());
		assert_eq!(cookie_names("u_person+linux+bad/path+space", 1), vec!["linux"]);
	}

	#[test]
	fn ratios_interleave_deduplicate_and_fill_small_community_sets() {
		for level in ["low", "balanced", "high"] {
			let percent = percentage(level);
			let d_count = PAGE_SIZE * percent / 100;
			let favorite: Vec<(String, String, bool)> = (0..PAGE_SIZE - d_count).map(|i| (format!("f{i}"), format!("fcommunity{}", i / 5), false)).collect();
			let discovery: Vec<(String, String, bool)> = (0..d_count).map(|i| (format!("d{i}"), format!("dcommunity{}", i / 5), true)).collect();
			let mixed = blend(favorite, discovery, percent, |e| &e.0, |e| &e.1);
			assert_eq!(mixed.len(), PAGE_SIZE);
			assert_eq!(mixed.iter().filter(|e| e.2).count(), d_count);
			assert!(mixed.iter().take(5).any(|e| e.2));
		}
		let items: Vec<(String, String)> = (0..25).map(|i| (i.to_string(), "linux".to_string())).collect();
		assert_eq!(blend(items.clone(), vec![items[0].clone()], 30, |e| &e.0, |e| &e.1).len(), 25);
		let mixed = blend(vec![("same", "linux"), ("other", "linux")], vec![("same", "space")], 50, |e| e.0, |e| e.1);
		assert_eq!(mixed.len(), 2);
	}

	#[test]
	fn cursors_are_independent_encoded_and_exhaustion_never_restarts() {
		assert!(valid_cursor("t3_abc123"));
		assert!(valid_cursor(END));
		assert!(!valid_cursor("t3_x&limit=100"));
		let url = page_url("top", END, "t3_abc123");
		assert_eq!(url, "/for-you?sort=top&f_after=end&d_after=t3_abc123&t=day");
		let path = listing_path(&["linux".into(), "space".into()], "top", "t3_abc", 7);
		assert_eq!(path, "/r/linux%2Bspace/top.json?raw_json=1&limit=7&after=t3_abc&t=day");
	}

	#[tokio::test]
	async fn post_validation_is_atomic_and_get_cannot_mutate() {
		let req = Request::builder()
			.method(Method::POST)
			.uri("/for-you/settings")
			.body(Body::from("favorites=linux&excluded=bad%2Fname&discovery_level=high"))
			.unwrap();
		let response = configure(req).await.unwrap();
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(!response.headers().contains_key("set-cookie"));
		let response = hide(Request::new(Body::empty())).await.unwrap();
		assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
		assert!(!response.headers().contains_key("set-cookie"));
	}

	#[tokio::test]
	async fn saving_is_separate_and_cookie_attributes_are_safe() {
		let req = Request::builder()
			.method(Method::POST)
			.uri("/for-you/settings")
			.body(Body::from("favorites=r%2FLinux&excluded=space&discovery_level=low"))
			.unwrap();
		let response = configure(req).await.unwrap();
		assert_eq!(response.headers()["location"], "/for-you?saved=1");
		let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().map(|h| h.to_str().unwrap()).collect();
		assert_eq!(cookies.len(), 3);
		for cookie in cookies {
			assert!(cookie.starts_with("fy_"));
			assert!(cookie.contains("HttpOnly"));
			assert!(cookie.contains("SameSite=Lax"));
			assert!(cookie.contains("Path=/"));
			assert!(cookie.contains("Expires="));
		}
	}

	#[test]
	fn favorites_fallback_and_filters_are_case_insensitive() {
		let req = Request::builder()
			.header("cookie", "subscriptions=Linux+u_person+Space; filters=SPACE+u_Alice; fy_excluded=Books")
			.body(Body::empty())
			.unwrap();
		assert_eq!(favorites(&req), vec!["linux", "space"]);
		assert_eq!(exclusions(&req), HashSet::from(["space".into(), "u_alice".into(), "books".into()]));
		let req = Request::builder().header("cookie", "subscriptions=Linux; fy_favorites=").body(Body::empty()).unwrap();
		assert!(favorites(&req).is_empty());
	}

	#[tokio::test]
	async fn normal_post_filters_and_nsfw_rules_are_preserved() {
		let post = |community: &str, author: &str, nsfw: bool| serde_json::json!({"data": {"id": "abc", "subreddit": community, "author": author, "over_18": nsfw}});
		let allowed = HashSet::from(["linux".into(), "space".into()]);
		let filters = HashSet::from(["space".into(), "u_alice".into()]);
		let mut posts = vec![
			crate::utils::parse_post(&post("Linux", "Alice", false)).await,
			crate::utils::parse_post(&post("SPACE", "Bob", false)).await,
			crate::utils::parse_post(&post("Linux", "Bob", true)).await,
			crate::utils::parse_post(&post("Linux", "Bob", false)).await,
		];
		clean_posts(&mut posts, &filters, false, &allowed);
		assert_eq!(posts.len(), 1);
		assert_eq!(posts[0].author.name, "Bob");
		assert!(!posts[0].flags.nsfw);
		let mut posts = vec![crate::utils::parse_post(&post("Linux", "Bob", true)).await];
		clean_posts(&mut posts, &HashSet::new(), true, &allowed);
		assert_eq!(posts.len(), 1);
	}

	#[tokio::test]
	async fn oversized_forms_and_full_hide_lists_cannot_change_cookies() {
		let req = Request::builder().method(Method::POST).body(Body::from("x".repeat(MAX_BODY + 1))).unwrap();
		let response = configure(req).await.unwrap();
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(!response.headers().contains_key("set-cookie"));
		let excluded = (0..100).map(|i| format!("community{i}")).collect::<Vec<_>>().join("+");
		let req = Request::builder()
			.method(Method::POST)
			.header("cookie", format!("fy_excluded={excluded}"))
			.body(Body::from("community=linux"))
			.unwrap();
		let response = hide(req).await.unwrap();
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(!response.headers().contains_key("set-cookie"));
	}

	#[tokio::test]
	async fn exhausted_or_empty_sources_do_not_fetch() {
		assert_eq!(listing(&[], "hot", "", 25).await.unwrap().1, END);
		assert_eq!(listing(&["linux".into()], "hot", END, 25).await.unwrap().1, END);
	}

	#[test]
	fn search_results_keep_established_sfw_public_communities() {
		let sub = |name: &str, subscribers: u64, over18: bool, kind: &str| serde_json::json!({"data": {"display_name": name, "subscribers": subscribers, "over18": over18, "subreddit_type": kind}});
		let value = serde_json::json!({"data": {"children": [
			sub("Cats", 5_000_000, false, "public"),       // the seed itself
			sub("CatsBeingCats", 400_000, false, "public"),
			sub("catgifs", 900, false, "public"),          // too small
			sub("catsnsfw", 200_000, true, "public"),      // NSFW
			sub("secretcats", 200_000, false, "private"), // not public
			sub("bad/name", 200_000, false, "public"),     // invalid
			sub("CatsOnCats", 120_000, false, "public"),
			sub("catswithjobs", 110_000, false, "public"),
			sub("cattaps", 100_000, false, "public"),
		]}});
		assert_eq!(parse_search_results(&value, "cats", 3), vec!["catsbeingcats", "catsoncats", "catswithjobs"]);
		assert!(parse_search_results(&serde_json::json!({}), "cats", 3).is_empty());
		assert_eq!(search_terms("ask_science"), "ask science");
	}

	fn cookie_values(response: &Response<Body>) -> HashMap<String, String> {
		// `ResponseExt::cookies` reads `Cookie`; responses set cookies via `Set-Cookie`.
		response
			.headers()
			.get_all("set-cookie")
			.iter()
			.filter_map(|h| Cookie::parse(h.to_str().ok()?.to_string()).ok())
			.map(|c| (c.name().to_string(), c.value().to_string()))
			.collect()
	}

	fn post_request(cookie: &str, form: &str) -> Request<Body> {
		Request::builder().method(Method::POST).header("cookie", cookie).body(Body::from(form.to_string())).unwrap()
	}

	#[tokio::test]
	async fn all_rating_actions_are_explicit_atomic_independent_and_idempotent() {
		let cookie = "fy_interested=Linux+linux+Books; fy_less_interested=LINUX+Space; fy_excluded=linux+Cooking; subscriptions=linux; fy_favorites=books; filters=space";
		for (rating, selected) in [("interested", Some(0)), ("less", Some(1)), ("exclude", Some(2)), ("clear", None)] {
			let form = format!("community=r%2FLinux&rating={rating}&return_to=%2Ffor-you%3Ff_after%3Dt3_ab%26d_after%3Dend%23post_ab");
			let response = rate(post_request(cookie, &form)).await.unwrap();
			assert_eq!(response.status(), StatusCode::SEE_OTHER);
			assert_eq!(response.headers()["location"], "/for-you?f_after=t3_ab&d_after=end#post_ab");
			let values = cookie_values(&response);
			assert_eq!(values.len(), 3);
			for (index, (name, _)) in RATING_COOKIES.iter().enumerate() {
				let names = cookie_names(&values[*name], 100);
				assert_eq!(names.iter().filter(|n| *n == "linux").count(), usize::from(selected == Some(index)));
				assert!(names.contains(&["books", "space", "cooking"][index].to_string()));
			}
			for header in response.headers().get_all("set-cookie") {
				let header = header.to_str().unwrap();
				assert!(header.contains("HttpOnly") && header.contains("SameSite=Lax") && header.contains("Path=/") && header.contains("Expires="));
				assert!(!header.starts_with("subscriptions=") && !header.starts_with("fy_favorites=") && !header.starts_with("filters="));
			}
			let next_cookie = values.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; ");
			assert_eq!(cookie_values(&rate(post_request(&next_cookie, &form)).await.unwrap()), values);
		}
	}

	#[tokio::test]
	async fn rating_failures_never_write_cookies() {
		for (cookie, form) in [
			("", "community=u_someone&rating=interested"),
			("", "community=bad%2Fname&rating=interested"),
			("", "community=linux&rating=unknown"),
			("", "community=linux"),
			("", "rating=clear"),
			("fy_interested=bad/path", "community=linux&rating=clear"),
			("fy_less_interested=u_someone", "community=linux&rating=interested"),
			("fy_excluded=bad/path", "community=linux&rating=less"),
		] {
			let response = rate(post_request(cookie, form)).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cookie} {form}");
			assert!(cookie_values(&response).is_empty());
		}
		for (name, max) in RATING_COOKIES {
			let full = (0..max).map(|i| format!("community{i}")).collect::<Vec<_>>().join("+");
			let rating = match name {
				"fy_interested" => "interested",
				"fy_less_interested" => "less",
				_ => "exclude",
			};
			let response = rate(post_request(&format!("{name}={full}"), &format!("community=linux&rating={rating}"))).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST);
			assert!(cookie_values(&response).is_empty());
			// At the cap, an existing choice remains valid.
			assert_eq!(
				rate(post_request(&format!("{name}={full}"), &format!("community=community0&rating={rating}")))
					.await
					.unwrap()
					.status(),
				StatusCode::SEE_OTHER
			);
			let over = format!("{name}={full}+overflow");
			let response = rate(post_request(&over, "community=community0&rating=clear")).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST);
			assert!(cookie_values(&response).is_empty());
		}
		for declared_length in [None, Some(MAX_BODY + 1)] {
			let mut req = post_request("", &"x".repeat(MAX_BODY + 1));
			if let Some(length) = declared_length {
				req.headers_mut().insert("content-length", length.to_string().parse().unwrap());
			}
			let response = rate(req).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST);
			assert!(cookie_values(&response).is_empty());
		}
		for endpoint in 0..3 {
			let req = Request::new(Body::from("community=linux&rating=interested"));
			let response = match endpoint {
				0 => rate(req).await,
				1 => hide(req).await,
				_ => configure(req).await,
			}
			.unwrap();
			assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
			assert!(cookie_values(&response).is_empty());
		}
	}

	#[tokio::test]
	async fn rating_redirects_reject_external_ambiguous_and_html_paths() {
		for path in [
			"https://evil.test/",
			"//evil.test/",
			"/\\evil.test",
			"/x\n",
			"/x\t",
			"/x y",
			"/\"onload=evil",
			"/'evil",
			"/<script>",
			"javascript:evil",
			"",
		] {
			assert_eq!(safe_return_to(path), "/for-you", "{path:?}");
			let mut form = form_urlencoded::Serializer::new(String::new());
			form.append_pair("community", "linux").append_pair("rating", "interested").append_pair("return_to", path);
			let form = form.finish();
			let response = rate(post_request("", &form)).await.unwrap();
			assert_eq!(response.headers()["location"], "/for-you");
			let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
			assert!(!String::from_utf8_lossy(&body).contains("evil"));
		}
		assert_eq!(safe_return_to(&format!("/{}", "a".repeat(2048))), "/for-you");
		assert_eq!(safe_return_to("/r/linux?sort=new&after=t3_ab#post_ab"), "/r/linux?sort=new&after=t3_ab#post_ab");
	}

	#[tokio::test]
	async fn csrf_guards_all_cookie_mutations_and_accepts_same_origin_ports() {
		for endpoint in 0..3 {
			for (header, value) in [
				("sec-fetch-site", "cross-site"),
				("origin", "https://evil.test"),
				("origin", "null"),
				("origin", "https://redlib.test:444"),
				("origin", "https://user@redlib.test"),
				("origin", "https://redlib.test/path"),
				("origin", "https:redlib.test"),
				("origin", "http://evil.test"),
				("origin", "https://redlib.test\\@evil.test"),
			] {
				let mut req = post_request("", "community=linux&rating=interested&favorites=linux&excluded=&discovery_level=low");
				req.headers_mut().insert("host", "redlib.test".parse().unwrap());
				req
					.headers_mut()
					.insert(hyper::header::HeaderName::from_bytes(header.as_bytes()).unwrap(), value.parse().unwrap());
				let response = match endpoint {
					0 => rate(req).await,
					1 => hide(req).await,
					_ => configure(req).await,
				}
				.unwrap();
				assert_eq!(response.status(), StatusCode::FORBIDDEN, "{endpoint} {header}={value}");
				assert!(cookie_values(&response).is_empty());
			}
		}
		for (origin, host) in [
			("https://REDLIB.test", "redlib.test:443"),
			("http://redlib.test:80", "REDLIB.test"),
			("https://redlib.test:8443", "redlib.test:8443"),
			("https://[::1]", "[::1]:443"),
		] {
			let req = Request::builder()
				.method(Method::POST)
				.header("host", host)
				.header("origin", origin)
				.body(Body::from("community=linux&rating=less"))
				.unwrap();
			assert_eq!(rate(req).await.unwrap().status(), StatusCode::SEE_OTHER);
		}
		let req = Request::builder().header("origin", "https://redlib.test").body(Body::empty()).unwrap();
		assert!(mutation_forbidden(&req));
		let req = Request::builder().header("host", "redlib.test").header("origin", "https://redlib.test").header("origin", "https://evil.test").body(Body::empty()).unwrap();
		assert!(mutation_forbidden(&req));
		let req = Request::builder().header("host", "redlib.test").header("origin", "https://redlib.test").header("sec-fetch-site", "cross-site").body(Body::empty()).unwrap();
		assert!(mutation_forbidden(&req));
		let req = Request::builder().header("sec-fetch-site", "same-origin").body(Body::empty()).unwrap();
		assert!(!mutation_forbidden(&req));
		// Reverse proxies may rewrite Host; browser fetch metadata still decides.
		let req = Request::builder()
			.header("host", "127.0.0.1:8080")
			.header("origin", "https://redlib.example")
			.header("sec-fetch-site", "same-origin")
			.body(Body::empty())
			.unwrap();
		assert!(!mutation_forbidden(&req));
		let req = Request::builder()
			.header("host", "127.0.0.1:8080")
			.header("x-forwarded-host", "redlib.example, proxy.internal")
			.header("origin", "https://redlib.example")
			.body(Body::empty())
			.unwrap();
		assert!(!mutation_forbidden(&req));
		let req = Request::builder()
			.header("host", "127.0.0.1:8080")
			.header("x-forwarded-host", "redlib.example")
			.header("origin", "https://evil.test")
			.body(Body::empty())
			.unwrap();
		assert!(mutation_forbidden(&req));
	}

	#[test]
	fn interests_extend_favorites_and_subscription_fallback_without_truncation() {
		for cookie in [
			"subscriptions=Linux+Space; fy_interested=SPACE+Books",
			"fy_favorites=linux+space; fy_interested=SPACE+Books",
		] {
			assert_eq!(seed_union(&post_request(cookie, "")), vec!["linux", "space", "books"]);
		}
		assert_eq!(seed_union(&post_request("subscriptions=space; fy_favorites=; fy_interested=Linux", "")), vec!["linux"]);
		let first = (0..50).map(|i| format!("community{i}")).collect::<Vec<_>>().join("+");
		let second = (50..100).map(|i| format!("community{i}")).collect::<Vec<_>>().join("+");
		let req = post_request(&format!("fy_favorites={first}; fy_interested={second}; fy_excluded=community99"), "");
		assert_eq!(seed_union(&req).len(), 100);
		let active: Vec<_> = seed_union(&req).into_iter().filter(|s| !exclusions(&req).contains(s)).collect();
		assert_eq!(active.len(), 99);
		// A catalog interest beyond the old 50-seed cutoff still supplies suggestions.
		let mut seeds = (0..50).map(|i| format!("community{i}")).collect::<Vec<_>>();
		seeds.push("linux".to_string());
		assert!(!candidates(&seeds, &HashSet::new()).is_empty());
		let req = post_request("fy_interested=Linux+LINUX+u_person+bad/path; fy_less_interested=Space+SPACE; fy_excluded=Books", "");
		assert_eq!(rating_lists(&req), (vec!["linux".into()], vec!["space".into()], vec!["books".into()]));
	}

	#[tokio::test]
	async fn less_interested_keeps_a_stable_quarter_in_mixed_and_single_sources() {
		let less = HashSet::from(["linux".to_string()]);
		for mixed in [false, true] {
			let mut posts = Vec::new();
			for i in 0..36 {
				let id = format!("{:x}", i); // all valid base36, residues cycle in groups of 4
				posts.push(crate::utils::parse_post(&serde_json::json!({"data": {"id": id, "subreddit": "LiNuX", "author": "Bob"}})).await);
				if mixed {
					posts.push(crate::utils::parse_post(&serde_json::json!({"data": {"id": format!("s{id}"), "subreddit": "Space", "author": "Bob"}})).await);
				}
			}
			clean_posts(&mut posts, &HashSet::new(), false, &HashSet::from(["linux".into(), "space".into()]));
			thin_less_interested(&mut posts, &less);
			assert_eq!(posts.iter().filter(|p| p.community == "LiNuX").count(), 9);
			assert_eq!(posts.iter().filter(|p| p.community == "Space").count(), if mixed { 36 } else { 0 });
			let ids: Vec<_> = posts.iter().map(|p| p.id.clone()).collect();
			thin_less_interested(&mut posts, &less);
			assert_eq!(posts.iter().map(|p| p.id.clone()).collect::<Vec<_>>(), ids);
		}
		let mut posts = Vec::new();
		for id in ["", "-1", "bad!", "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"] {
			posts.push(crate::utils::parse_post(&serde_json::json!({"data": {"id": id, "subreddit": "Linux"}})).await);
		}
		thin_less_interested(&mut posts, &less);
		assert_eq!(posts.len(), 4);
	}

	#[tokio::test]
	async fn legacy_hide_reconciles_ratings_and_keeps_saved_notice() {
		let response = hide(post_request(
			"fy_interested=Linux+Books; fy_less_interested=linux+space; fy_excluded=cooking",
			"community=LINUX",
		))
		.await
		.unwrap();
		assert_eq!(response.headers()["location"], "/for-you?saved=hidden");
		let values = cookie_values(&response);
		assert_eq!(values["fy_interested"], "books");
		assert_eq!(values["fy_less_interested"], "space");
		assert_eq!(values["fy_excluded"], "cooking+linux");
		assert!(saved_notice(&HashMap::from([("saved".into(), "hidden".into())])).contains("For You"));
	}

	#[tokio::test]
	async fn settings_preserve_optional_ratings_and_resolve_conflicts_atomically() {
		let old_form = "favorites=linux&excluded=&discovery_level=high";
		let response = configure(post_request("fy_interested=Books; fy_less_interested=Space", old_form)).await.unwrap();
		assert_eq!(cookie_values(&response).len(), 3);
		let response = configure(post_request(
			"fy_interested=Books; fy_less_interested=Space",
			"favorites=linux&excluded=books&discovery_level=high",
		))
		.await
		.unwrap();
		assert_eq!(cookie_values(&response)["fy_interested"], "");
		assert!(!cookie_values(&response).contains_key("fy_less_interested"));
		let response = configure(post_request(
			"",
			"favorites=linux&interested=LINUX%2CBooks%2CSpace&less_interested=space%2Cbooks&excluded=books&discovery_level=low",
		))
		.await
		.unwrap();
		let values = cookie_values(&response);
		assert_eq!(values["fy_favorites"], "linux");
		assert_eq!(values["fy_interested"], "linux");
		assert_eq!(values["fy_less_interested"], "space");
		assert_eq!(values["fy_excluded"], "books");
		for form in [
			"favorites=linux&interested=bad%2Fname&excluded=&discovery_level=low",
			"favorites=linux&less_interested=u_person&excluded=&discovery_level=low",
			"favorites=linux&interested=books&excluded=&discovery_level=invalid",
		] {
			let response = configure(post_request("", form)).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST);
			assert!(cookie_values(&response).is_empty());
		}
		let response = configure(post_request("fy_interested=bad/path", old_form)).await.unwrap();
		assert_eq!(response.status(), StatusCode::BAD_REQUEST);
		assert!(cookie_values(&response).is_empty());
		for (field, max) in [("interested", 50), ("less_interested", 100)] {
			let names = (0..=max).map(|i| format!("community{i}")).collect::<Vec<_>>().join(",");
			let response = configure(post_request("", &format!("{old_form}&{field}={names}"))).await.unwrap();
			assert_eq!(response.status(), StatusCode::BAD_REQUEST);
			assert!(cookie_values(&response).is_empty());
		}
		let view = setup(&post_request("fy_interested=Linux+Books; fy_less_interested=Space", ""), String::new());
		assert_eq!(view.interested, "linux, books");
		assert_eq!(view.less_interested, "space");
	}

	#[test]
	fn ratings_export_query_and_apply_normalize_cap_and_preserve_legacy_layout() {
		let form = form_urlencoded::parse(b"fy_interested=Linux%2BLINUX%2Bu_person%2Bbad%2Fname&fy_less_interested=r%2FSpace").collect();
		let ratings = ForYouRatings::from_query(&form);
		assert_eq!(
			ratings,
			ForYouRatings {
				interested: "linux".into(),
				less_interested: "space".into()
			}
		);
		assert_eq!(ratings.to_query(), "fy_interested=linux&fy_less_interested=space");
		assert_eq!(
			ForYouRatings::from_request(&post_request("fy_interested=Linux+LINUX; fy_less_interested=Space", "")),
			ratings
		);
		assert!(ForYouRatings::default().to_query().is_empty());
		let untrusted = ForYouRatings {
			interested: (0..70).map(|i| format!("Community{i}")).collect::<Vec<_>>().join("+"),
			less_interested: "Space+SPACE+u_person+bad/path".into(),
		};
		let mut response = Response::new(Body::empty());
		untrusted.apply(&mut response, false);
		assert_eq!(cookie_names(&cookie_values(&response)["fy_interested"], 100).len(), 50);
		assert_eq!(cookie_values(&response)["fy_less_interested"], "space");
		let roundtrip = ForYouRatings::from_query(&form_urlencoded::parse(untrusted.to_query().as_bytes()).collect());
		assert_eq!(roundtrip.interested.split('+').count(), 50);
		let query = format!("fy_less_interested={}", (0..120).map(|i| format!("Community{i}")).collect::<Vec<_>>().join("%2B"));
		let capped = ForYouRatings::from_query(&form_urlencoded::parse(query.as_bytes()).collect());
		assert_eq!(capped.less_interested.split('+').count(), 100);
		assert!(capped.less_interested.starts_with("community0+"));
		let mut response = Response::new(Body::empty());
		ForYouRatings::default().apply(&mut response, false);
		assert!(cookie_values(&response).is_empty());
		ForYouRatings::default().apply(&mut response, true);
		assert_eq!(
			cookie_values(&response),
			HashMap::from([("fy_interested".into(), "".into()), ("fy_less_interested".into(), "".into())])
		);
		let mut response = Response::new(Body::empty());
		ForYouRatings {
			interested: "Linux".into(),
			less_interested: String::new(),
		}
		.apply(&mut response, false);
		assert_eq!(cookie_values(&response), HashMap::from([("fy_interested".into(), "linux".into())]));
		// Binary field order and types of the legacy export remain unchanged.
		let legacy = ForYouExport {
			favorites: Some("linux".into()),
			excluded: "space".into(),
			level: "low".into(),
		};
		assert_eq!(
			bincode::serialize(&legacy).unwrap(),
			bincode::serialize(&(Some("linux".to_string()), "space".to_string(), "low".to_string())).unwrap()
		);
	}

	#[test]
	fn subscribing_updates_saved_favorites_only() {
		let subscribe = |cookie: &str, names: &[&str], add: bool| {
			let req = Request::builder().header("cookie", cookie).body(Body::empty()).unwrap();
			let mut response = Response::new(Body::empty());
			let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
			sync_favorites(&req, &mut response, &names, add);
			response.headers().get_all("set-cookie").iter().map(|h| h.to_str().unwrap().to_string()).collect::<Vec<_>>()
		};
		let added = subscribe("fy_favorites=linux+space", &["Cooking"], true);
		assert!(added.iter().any(|c| c.starts_with("fy_favorites=linux+space+cooking;")), "{added:?}");
		let removed = subscribe("fy_favorites=linux+space", &["Space"], false);
		assert!(removed.iter().any(|c| c.starts_with("fy_favorites=linux;")), "{removed:?}");
		// Already present, user feeds, or no saved list: no cookie written.
		assert!(subscribe("fy_favorites=linux", &["Linux"], true).is_empty());
		assert!(subscribe("fy_favorites=linux", &["u_someone"], true).is_empty());
		assert!(subscribe("subscriptions=linux", &["cooking"], true).is_empty());
	}
}
