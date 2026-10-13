#![allow(clippy::cmp_owned)]

use std::collections::HashMap;

// CRATES
use crate::discovery::ForYouExport;
use crate::server::ResponseExt;
use crate::subreddit::join_until_size_limit;
use crate::utils::{deflate_compress, deflate_decompress, redirect, template, Preferences};
use askama::Template;
use cookie::Cookie;
use futures_lite::StreamExt;
use hyper::{Body, Request, Response};
use time::{Duration, OffsetDateTime};
use tokio::time::timeout;
use url::form_urlencoded;

// STRUCTS
#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
	prefs: Preferences,
	url: String,
	/// Restore-link query including For You settings.
	restore_query: Result<String, String>,
	/// Portable export code including For You settings.
	export_code: Result<String, String>,
}

/// Joins two query strings, skipping empty parts.
fn join_query(a: &str, b: &str) -> String {
	match (a.is_empty(), b.is_empty()) {
		(_, true) => a.to_string(),
		(true, false) => b.to_string(),
		(false, false) => format!("{a}&{b}"),
	}
}

/// Export code: compressed bincode of `(Preferences, ForYouExport)`.
/// `Preferences` bytes come first and are unchanged, so upstream Redlib (which
/// ignores trailing bytes) can still import these codes, minus For You.
pub fn export_code(prefs: &Preferences, for_you: &ForYouExport) -> Result<String, String> {
	let bytes = bincode::serialize(&(prefs, for_you)).map_err(|e| e.to_string())?;
	Ok(base2048::encode(&deflate_compress(bytes)?))
}

/// Decodes new codes, falling back to older codes that contain only `Preferences`.
pub fn decode_export(bytes: &[u8]) -> Result<(Preferences, ForYouExport), String> {
	bincode::deserialize::<(Preferences, ForYouExport)>(bytes).or_else(|_| {
		bincode::deserialize::<Preferences>(bytes)
			.map(|prefs| (prefs, ForYouExport::default()))
			.map_err(|e| format!("Failed to deserialize bytes into Preferences struct: {e}"))
	})
}

// CONSTANTS

const PREFS: [&str; 19] = [
	"theme",
	"front_page",
	"layout",
	"wide",
	"comment_sort",
	"post_sort",
	"blur_spoiler",
	"show_nsfw",
	"blur_nsfw",
	"use_hls",
	"hide_hls_notification",
	"autoplay_videos",
	"hide_sidebar_and_summary",
	"fixed_navbar",
	"hide_awards",
	"hide_score",
	"disable_visit_reddit_confirmation",
	"video_quality",
	"remove_default_feeds",
];

// FUNCTIONS

/// Retrieve cookies from request "Cookie" header
pub async fn get(req: Request<Body>) -> Result<Response<Body>, String> {
	let url = req.uri().to_string();
	let prefs = Preferences::new(&req);
	let for_you = ForYouExport::from_request(&req);
	Ok(template(&SettingsTemplate {
		restore_query: prefs.to_urlencoded().map(|query| join_query(&query, &for_you.to_query())),
		export_code: export_code(&prefs, &for_you),
		prefs,
		url,
	}))
}

/// Set cookies using response "Set-Cookie" header
pub async fn set(req: Request<Body>) -> Result<Response<Body>, String> {
	// Split the body into parts
	let (parts, mut body) = req.into_parts();

	// Grab existing cookies
	let _cookies: Vec<Cookie<'_>> = parts
		.headers
		.get_all("Cookie")
		.iter()
		.filter_map(|header| Cookie::parse(header.to_str().unwrap_or_default()).ok())
		.collect();

	// Aggregate the body...
	// let whole_body = hyper::body::aggregate(req).await.map_err(|e| e.to_string())?;
	let body_bytes = body
		.try_fold(Vec::new(), |mut data, chunk| {
			data.extend_from_slice(&chunk);
			Ok(data)
		})
		.await
		.map_err(|e| e.to_string())?;

	let form = url::form_urlencoded::parse(&body_bytes).collect::<HashMap<_, _>>();

	let mut response = redirect("/settings?saved=1");

	for &name in &PREFS {
		match form.get(name) {
			Some(value) => response.insert_cookie(
				Cookie::build((name.to_owned(), value.clone()))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			),
			None => response.remove_cookie(name.to_string()),
		};
	}

	Ok(response)
}

fn set_cookies_method(req: Request<Body>, remove_cookies: bool) -> Response<Body> {
	// Split the body into parts
	let (parts, _) = req.into_parts();

	// Grab existing cookies
	let _cookies: Vec<Cookie<'_>> = parts
		.headers
		.get_all("Cookie")
		.iter()
		.filter_map(|header| Cookie::parse(header.to_str().unwrap_or_default()).ok())
		.collect();

	let query = parts.uri.query().unwrap_or_default().as_bytes();

	let form = url::form_urlencoded::parse(query).collect::<HashMap<_, _>>();

	let path = match form.get("redirect") {
		Some(value) => {
			let value = value.replace("%26", "&").replace("%23", "#");
			if value.starts_with('/') {
				value
			} else {
				format!("/{value}")
			}
		}
		None => "/".to_string(),
	};

	let mut response = redirect(&path);

	for name in PREFS {
		match form.get(name) {
			Some(value) => response.insert_cookie(
				Cookie::build((name.to_owned(), value.clone()))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			),
			None => {
				if remove_cookies {
					response.remove_cookie(name.to_string());
				}
			}
		};
	}

	// For You settings (restore links and decoded export codes).
	ForYouExport::from_query(&form).apply(&mut response, remove_cookies);

	// Get subscriptions/filters to restore from query string
	let subscriptions = form.get("subscriptions");
	let filters = form.get("filters");

	// We can't search through the cookies directly like in subreddit.rs, so instead we have to make a string out of the request's headers to search through
	let cookies_string = parts
		.headers
		.get("cookie")
		.map(|hv| hv.to_str().unwrap_or("").to_string()) // Return String
		.unwrap_or_else(String::new); // Return an empty string if None

	// If there are subscriptions to restore set them and delete any old subscriptions cookies, otherwise delete them all
	if let Some(subscriptions) = subscriptions {
		let sub_list: Vec<String> = subscriptions.split('+').map(str::to_string).collect();

		// Start at 0 to keep track of what number we need to start deleting old subscription cookies from
		let mut subscriptions_number_to_delete_from = 0;

		// Starting at 0 so we handle the subscription cookie without a number first
		for (subscriptions_number, list) in join_until_size_limit(&sub_list).into_iter().enumerate() {
			let subscriptions_cookie = if subscriptions_number == 0 {
				"subscriptions".to_string()
			} else {
				format!("subscriptions{subscriptions_number}")
			};

			response.insert_cookie(
				Cookie::build((subscriptions_cookie, list))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			);

			subscriptions_number_to_delete_from += 1;
		}

		// While subscriptionsNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("subscriptions{subscriptions_number_to_delete_from}=")) {
			// Remove that subscriptions cookie
			response.remove_cookie(format!("subscriptions{subscriptions_number_to_delete_from}"));

			// Increment subscriptions cookie number
			subscriptions_number_to_delete_from += 1;
		}
	} else {
		// Remove unnumbered subscriptions cookie
		response.remove_cookie("subscriptions".to_string());

		// Starts at one to deal with the first numbered subscription cookie and onwards
		let mut subscriptions_number_to_delete_from = 1;

		// While subscriptionsNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("subscriptions{subscriptions_number_to_delete_from}=")) {
			// Remove that subscriptions cookie
			response.remove_cookie(format!("subscriptions{subscriptions_number_to_delete_from}"));

			// Increment subscriptions cookie number
			subscriptions_number_to_delete_from += 1;
		}
	}

	// If there are filters to restore set them and delete any old filters cookies, otherwise delete them all
	if let Some(filters) = filters {
		let filters_list: Vec<String> = filters.split('+').map(str::to_string).collect();

		// Start at 0 to keep track of what number we need to start deleting old subscription cookies from
		let mut filters_number_to_delete_from = 0;

		// Starting at 0 so we handle the subscription cookie without a number first
		for (filters_number, list) in join_until_size_limit(&filters_list).into_iter().enumerate() {
			let filters_cookie = if filters_number == 0 {
				"filters".to_string()
			} else {
				format!("filters{filters_number}")
			};

			response.insert_cookie(
				Cookie::build((filters_cookie, list))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			);

			filters_number_to_delete_from += 1;
		}

		// While filtersNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("filters{filters_number_to_delete_from}=")) {
			// Remove that filters cookie
			response.remove_cookie(format!("filters{filters_number_to_delete_from}"));

			// Increment filters cookie number
			filters_number_to_delete_from += 1;
		}
	} else {
		// Remove unnumbered filters cookie
		response.remove_cookie("filters".to_string());

		// Starts at one to deal with the first numbered subscription cookie and onwards
		let mut filters_number_to_delete_from = 1;

		// While filtersNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("filters{filters_number_to_delete_from}=")) {
			// Remove that sfilters cookie
			response.remove_cookie(format!("filters{filters_number_to_delete_from}"));

			// Increment filters cookie number
			filters_number_to_delete_from += 1;
		}
	}

	response
}

/// Set cookies using response "Set-Cookie" header
pub async fn restore(req: Request<Body>) -> Result<Response<Body>, String> {
	Ok(set_cookies_method(req, true))
}

pub async fn update(req: Request<Body>) -> Result<Response<Body>, String> {
	Ok(set_cookies_method(req, false))
}

pub async fn encoded_restore(req: Request<Body>) -> Result<Response<Body>, String> {
	let body = hyper::body::to_bytes(req.into_body())
		.await
		.map_err(|e| format!("Failed to get bytes from request body: {e}"))?;

	if body.len() > 1024 * 1024 {
		return Err("Request body too large".to_string());
	}

	let encoded_prefs = form_urlencoded::parse(&body)
		.find(|(key, _)| key == "encoded_prefs")
		.map(|(_, value)| value)
		.ok_or_else(|| "encoded_prefs parameter not found in request body".to_string())?;

	let bytes = base2048::decode(&encoded_prefs).ok_or_else(|| "Failed to decode base2048 encoded preferences".to_string())?;

	let out = timeout(std::time::Duration::from_secs(1), async { deflate_decompress(bytes) })
		.await
		.map_err(|e| format!("Failed to decompress bytes: {e}"))??;

	let (mut prefs, for_you) = timeout(std::time::Duration::from_secs(1), async { decode_export(&out) })
		.await
		.map_err(|e| format!("Failed to deserialize preferences: {e}"))??;

	prefs.available_themes = vec![];

	let url = format!("/settings/restore/?{}", join_query(&prefs.to_urlencoded()?, &for_you.to_query()));

	Ok(redirect(&url))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_for_you() -> ForYouExport {
		ForYouExport {
			favorites: Some("linux+space".into()),
			excluded: "books".into(),
			level: "high".into(),
		}
	}

	#[test]
	fn export_codes_include_for_you_and_stay_compatible() {
		let prefs = Preferences {
			theme: "dark".into(),
			front_page: "for_you".into(),
			..Preferences::default()
		};
		let code = export_code(&prefs, &sample_for_you()).unwrap();
		let bytes = deflate_decompress(base2048::decode(&code).unwrap()).unwrap();

		// This fork restores both parts.
		let (decoded, for_you) = decode_export(&bytes).unwrap();
		assert_eq!(decoded.theme, "dark");
		assert_eq!(decoded.front_page, "for_you");
		assert_eq!(for_you, sample_for_you());

		// Upstream Redlib decodes only `Preferences` and ignores the trailing For You bytes.
		let upstream: Preferences = bincode::deserialize(&bytes).unwrap();
		assert_eq!(upstream.theme, "dark");

		// Older codes (Preferences only) still import, with For You left at defaults.
		let legacy = bincode::serialize(&prefs).unwrap();
		let (decoded, for_you) = decode_export(&legacy).unwrap();
		assert_eq!(decoded.theme, "dark");
		assert_eq!(for_you, ForYouExport::default());
	}

	#[test]
	fn restore_links_carry_validated_for_you_settings() {
		let query = sample_for_you().to_query();
		assert_eq!(query, "fy_favorites=linux%2Bspace&fy_excluded=books&fy_level=high");
		let req = Request::builder().uri(format!("/settings/restore/?theme=dark&{query}")).body(Body::empty()).unwrap();
		let response = set_cookies_method(req, true);
		let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().map(|h| h.to_str().unwrap().to_string()).collect();
		for expected in ["theme=dark;", "fy_favorites=linux+space;", "fy_excluded=books;", "fy_level=high;"] {
			assert!(cookies.iter().any(|c| c.starts_with(expected)), "missing {expected} in {cookies:?}");
		}

		// Untrusted values are normalized: bad names dropped, unknown levels ignored.
		let req = Request::builder()
			.uri("/settings/restore/?fy_favorites=Linux%2Bbad%2Fpath&fy_level=extreme")
			.body(Body::empty())
			.unwrap();
		let response = set_cookies_method(req, true);
		let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().map(|h| h.to_str().unwrap().to_string()).collect();
		assert!(cookies.iter().any(|c| c.starts_with("fy_favorites=linux;")));
		assert!(!cookies.iter().any(|c| c.starts_with("fy_level=extreme")));

		// Partial updates (e.g. /settings/update) never touch For You.
		let req = Request::builder().uri("/settings/update/?use_hls=on").body(Body::empty()).unwrap();
		let response = set_cookies_method(req, false);
		assert!(!response.headers().get_all("set-cookie").iter().any(|h| h.to_str().unwrap().starts_with("fy_")));
	}

	#[tokio::test]
	async fn saving_preferences_confirms_and_sets_cookies() {
		let request = Request::builder()
			.method("POST")
			.uri("/settings")
			.body(Body::from("theme=dark&autoplay_videos=off&use_hls=on"))
			.unwrap();
		let response = set(request).await.unwrap();
		assert!(response.status().is_redirection());
		assert_eq!(response.headers().get("location").unwrap(), "/settings?saved=1");
		let cookies: Vec<_> = response.headers().get_all("set-cookie").iter().map(|cookie| cookie.to_str().unwrap()).collect();
		for expected in ["theme=dark;", "autoplay_videos=off;", "use_hls=on;"] {
			assert!(cookies.iter().any(|cookie| cookie.starts_with(expected)), "Missing cookie: {expected}");
		}
	}
}
