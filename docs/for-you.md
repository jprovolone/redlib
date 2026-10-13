# For You feed

Visit `/for-you` or choose **For You** in navigation. Use **Curate** to configure it.

- Favorites start from your subscriptions until you save a separate list. Saving does not change subscriptions. Once a list is saved, subscribing to a community adds it and unsubscribing removes it.
- Discovery levels target 20%, 30%, or 50% related posts. Source availability, filters, duplicates, and NSFW settings can reduce the actual mix.
- Suggestions for favorites in `static/related-communities.json` come from those curated groups. For other favorites (up to 8 per load), Reddit's community search is used, keeping public, SFW communities with at least 10k members. Results are cached for a day. Each suggested post says which favorite led to it. No browsing history is used.
- Exclusions and existing user/community filters apply. **Hide r/…** adds a community to For You exclusions; remove it under Curate to undo.
- Lists are limited to 50 favorites and 100 exclusions, using community names separated by commas, whitespace, or `+`. User-profile feeds are not supported here.
- Settings are local to your browser, stored in `fy_favorites`, `fy_excluded`, and `fy_level` cookies. The settings code and restore link on the Settings page include them. Codes from this fork still import into upstream Redlib (For You is ignored there), and older codes still import here.

The feed fetches two independent Reddit listings and interleaves them. Pagination preserves separate source cursors; **First page** resets them. Filtering may leave short pages. Partial source failures are shown rather than silently replacing favorites with Popular.

## Branches and publishing

Development occurs on `feature/for-you-feed`, based on `ui/ios-reddit-redesign`. GHCR publishing remains restricted to the UI branch. After local review, merge the feature into that branch to include it in published UI images. Nothing publishes directly from the feature branch.
