//! Freshness for hot feeds, closer to the Reddit app: each load of a hot
//! listing also pulls a few `rising` and `new` posts from the same communities
//! and weaves them in at slightly different positions each time. Every source
//! keeps its own pagination cursor so endless scrolling keeps working.
use crate::utils::Post;
use std::collections::HashSet;
use url::form_urlencoded;

/// Extra posts per page on top of the normal hot page.
pub const RISING_LIMIT: usize = 6;
pub const NEW_LIMIT: usize = 3;
/// Cursor value for an exhausted source.
pub const END: &str = "end";

/// Cursors are either empty (first page), `end`, or a Reddit fullname.
pub fn valid_cursor(cursor: &str) -> bool {
	cursor.is_empty()
		|| cursor == END
		|| cursor
			.strip_prefix("t3_")
			.is_some_and(|id| !id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Cursors for the extra sources, read from `r_after` / `n_after` (or the
/// given prefixed names). Invalid values restart that source.
pub struct Cursors {
	pub rising: String,
	pub new: String,
}

impl Cursors {
	pub fn from_query(query: &str, rising_key: &str, new_key: &str) -> Self {
		let mut cursors = Self {
			rising: String::new(),
			new: String::new(),
		};
		for (key, value) in form_urlencoded::parse(query.as_bytes()) {
			if !valid_cursor(&value) {
				continue;
			}
			if key == rising_key {
				cursors.rising = value.into_owned();
			} else if key == new_key {
				cursors.new = value.into_owned();
			}
		}
		cursors
	}

	/// Query suffix (leading `&`) for the next page.
	pub fn to_query(&self, rising_key: &str, new_key: &str) -> String {
		let mut query = form_urlencoded::Serializer::new(String::new());
		query.append_pair(rising_key, &self.rising).append_pair(new_key, &self.new);
		format!("&{}", query.finish())
	}

	pub fn exhausted(&self) -> bool {
		self.rising == END && self.new == END
	}
}

/// Listing path for already-encoded community names (`a%2Bb`).
fn path(feed: &str, sort: &str, after: &str, limit: usize, extra_params: &str) -> String {
	let mut query = form_urlencoded::Serializer::new(String::new());
	query.append_pair("raw_json", "1").append_pair("limit", &limit.to_string());
	if !after.is_empty() {
		query.append_pair("after", after);
	}
	format!("/r/{feed}/{sort}.json?{}{extra_params}", query.finish())
}

async fn source(feed: &str, sort: &str, after: &str, limit: usize, quarantined: bool, extra_params: &str) -> (Vec<Post>, String) {
	if after == END {
		return (Vec::new(), END.to_string());
	}
	match Post::fetch(&path(feed, sort, after, limit, extra_params), quarantined).await {
		Ok((mut posts, next)) => {
			posts.truncate(limit);
			let next = if next.is_empty() || posts.is_empty() { END.to_string() } else { next };
			(posts, next)
		}
		// A failed extra source never breaks the page; retry it on the next page.
		Err(_) => (Vec::new(), after.to_string()),
	}
}

/// Fetches the rising and new extras concurrently. Returns the posts and the
/// cursors for the next page.
pub async fn extras(feed: &str, cursors: &Cursors, quarantined: bool, extra_params: &str) -> (Vec<Post>, Vec<Post>, Cursors) {
	let ((rising, rising_next), (new, new_next)) = tokio::join!(
		source(feed, "rising", &cursors.rising, RISING_LIMIT, quarantined, extra_params),
		source(feed, "new", &cursors.new, NEW_LIMIT, quarantined, extra_params)
	);
	(
		rising,
		new,
		Cursors {
			rising: rising_next,
			new: new_next,
		},
	)
}

/// Weaves rising and new items into a hot page. Pinned items stay first and hot
/// keeps its order; extras land roughly every 4th (rising) and 9th (new) slot
/// with jitter, so each refresh looks different. Duplicates are dropped, with
/// hot taking priority.
pub fn weave<T>(base: Vec<T>, rising: Vec<T>, new: Vec<T>, id: impl Fn(&T) -> &str, pinned: impl Fn(&T) -> bool, rng: &mut fastrand::Rng) -> Vec<T> {
	let mut seen: HashSet<String> = base.iter().map(|item| id(item).to_string()).collect();
	let mut fresh = |items: Vec<T>| -> Vec<T> { items.into_iter().filter(|item| seen.insert(id(item).to_string())).collect() };
	let rising = fresh(rising);
	let new = fresh(new);

	let mut result = Vec::with_capacity(base.len() + rising.len() + new.len());
	let mut base = base.into_iter().peekable();
	while let Some(item) = base.next_if(|item| pinned(item)) {
		result.push(item);
	}
	let mut rising = rising.into_iter();
	let mut new = new.into_iter();
	let mut until_rising = rng.usize(1..=3);
	let mut until_new = rng.usize(4..=8);
	loop {
		let mut progressed = false;
		if let Some(item) = base.next() {
			result.push(item);
			progressed = true;
		}
		until_rising = until_rising.saturating_sub(1);
		until_new = until_new.saturating_sub(1);
		if until_rising == 0 {
			if let Some(item) = rising.next() {
				result.push(item);
				progressed = true;
			}
			until_rising = rng.usize(3..=5);
		}
		if until_new == 0 {
			if let Some(item) = new.next() {
				result.push(item);
				progressed = true;
			}
			until_new = rng.usize(7..=11);
		}
		if !progressed && base.peek().is_none() {
			// Short hot page: append whatever extras remain.
			result.extend(rising);
			result.extend(new);
			break;
		}
	}
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ids(items: &[(&'static str, bool)]) -> Vec<&'static str> {
		items.iter().map(|i| i.0).collect()
	}

	#[test]
	fn weave_keeps_hot_order_pins_and_includes_every_unique_extra() {
		for seed in 0..50 {
			let mut rng = fastrand::Rng::with_seed(seed);
			let base = vec![("pin", true), ("h1", false), ("h2", false), ("h3", false), ("h4", false), ("h5", false), ("h6", false), ("h7", false), ("h8", false)];
			let rising = vec![("r1", false), ("h2", false), ("r2", false)];
			let new = vec![("n1", false), ("r1", false)];
			let mixed = weave(base, rising, new, |i| i.0, |i| i.1, &mut rng);
			let order = ids(&mixed);
			assert_eq!(order[0], "pin");
			assert_eq!(order.len(), 12, "{order:?}");
			let hot: Vec<_> = order.iter().filter(|i| i.starts_with('h')).copied().collect();
			assert_eq!(hot, ["h1", "h2", "h3", "h4", "h5", "h6", "h7", "h8"]);
			for extra in ["r1", "r2", "n1"] {
				assert_eq!(order.iter().filter(|i| **i == extra).count(), 1);
			}
			// Extras are spread out, not dumped at the end of a full page.
			assert!(order.iter().position(|i| *i == "r1").unwrap() < 6, "{order:?}");
		}
	}

	#[test]
	fn weave_handles_empty_sources() {
		let mut rng = fastrand::Rng::with_seed(1);
		let only_extras = weave(Vec::new(), vec![("r1", false), ("r2", false)], vec![("n1", false)], |i| i.0, |i| i.1, &mut rng);
		assert_eq!(ids(&only_extras), ["r1", "r2", "n1"]);
		let only_hot = weave(vec![("h1", false), ("h2", false)], Vec::new(), Vec::new(), |i| i.0, |i| i.1, &mut rng);
		assert_eq!(ids(&only_hot), ["h1", "h2"]);
	}

	#[test]
	fn cursors_round_trip_and_reject_injection() {
		let cursors = Cursors::from_query("sort=hot&r_after=t3_abc&n_after=end&after=t3_x", "r_after", "n_after");
		assert_eq!((cursors.rising.as_str(), cursors.new.as_str()), ("t3_abc", END));
		assert_eq!(cursors.to_query("r_after", "n_after"), "&r_after=t3_abc&n_after=end");
		let cursors = Cursors::from_query("r_after=t3_x%26limit%3D100&n_after=..%2F", "r_after", "n_after");
		assert!(cursors.rising.is_empty() && cursors.new.is_empty());
		assert!(Cursors {
			rising: END.into(),
			new: END.into()
		}
		.exhausted());
		assert_eq!(path("linux%2Bspace", "rising", "t3_a", 6, "&geo_filter=GLOBAL"), "/r/linux%2Bspace/rising.json?raw_json=1&limit=6&after=t3_a&geo_filter=GLOBAL");
	}
}
