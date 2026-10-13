pub mod client;
pub mod config;
pub mod discovery;
pub mod duplicates;
pub mod instance_info;
pub mod oauth;
pub mod oauth_resources;
pub mod post;
pub mod search;
pub mod server;
pub mod settings;
pub mod subreddit;
pub mod user;
pub mod utils;

#[cfg(test)]
mod asset_tests {
	/// Guards against truncated rules: one unclosed brace silently disables every rule after it.
	#[test]
	fn ios_stylesheet_braces_are_balanced() {
		let css = include_str!("../static/ios.css");
		let mut depth: i32 = 0;
		for (number, line) in css.lines().enumerate() {
			for ch in line.chars() {
				match ch {
					'{' => depth += 1,
					'}' => depth -= 1,
					_ => {}
				}
				assert!(depth >= 0, "Unexpected closing brace on line {}", number + 1);
			}
			// Every top-level rule in this stylesheet is a single line or a closed block.
			assert!(depth <= 2, "Brace nesting too deep on line {} (likely an unclosed rule)", number + 1);
		}
		assert_eq!(depth, 0, "ios.css has an unclosed block");
	}
}
