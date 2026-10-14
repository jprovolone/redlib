# For You feed

Visit `/for-you` or choose **For You** in navigation. Use **Curate** to configure it.

- Favorites start from your subscriptions until you save a separate list. Saving does not change subscriptions. Once a list is saved, subscribing to a community adds it and unsubscribing removes it.
- Discovery levels target 20%, 30%, or 50% related posts. Source availability, filters, duplicates, and NSFW settings can reduce the actual mix.
- Suggestions for favorites in `static/related-communities.json` come from those curated groups. For other favorites (up to 8 per load), Reddit's community search is used, keeping public, SFW communities with at least 10k members. Results are cached for a day. Each suggested post says which favorite led to it. No browsing history is used.
- Exclusions and existing user/community filters apply.
- Every post card has a rating menu at its top right (**⋯**). Ratings apply to the post's community, only affect For You, and never change subscriptions or global filters:
  - **Interested** adds the community to `fy_interested`. Interested communities are fetched alongside favorites and seed related suggestions, without subscribing.
  - **Not interested** adds it to `fy_less_interested`. Its posts are thinned to about 1 in 4 (chosen by post ID, so it is stable across reloads). It is not excluded.
  - **Exclude** adds it to `fy_excluded`, the same list the settings page edits.
  - **Clear rating** removes it from all three lists. A community has one rating at a time; the latest choice wins.
  - With JavaScript, rating happens in place; without it the form posts to `/feedback/rate` and returns to the post. The old `/for-you/hide` endpoint still works as **Exclude** for cached pages.
- Rating and For You settings forms reject cross-site submissions (`Sec-Fetch-Site: cross-site`, or a mismatched `Origin` when fetch metadata is absent).
- Lists are limited to 50 favorites, 50 interested, 100 shown-less, and 100 excluded communities, using community names separated by commas, whitespace, or `+`. User-profile feeds are not supported here.
- Settings are local to your browser, stored in `fy_favorites`, `fy_interested`, `fy_less_interested`, `fy_excluded`, and `fy_level` cookies. The settings code and restore link on the Settings page include them. Codes append each part after the previous one's bytes, so new codes still import into upstream Redlib and earlier versions of this fork (the parts they don't know are ignored), and older codes still import here.

The feed fetches two independent Reddit listings and interleaves them. Pagination preserves separate source cursors; **First page** resets them. Filtering may leave short pages. Partial source failures are shown rather than silently replacing favorites with Popular.

## Branches and publishing

Development occurs on `feature/for-you-feed`, based on `ui/ios-reddit-redesign`. GHCR publishing remains restricted to the UI branch. After local review, merge the feature into that branch to include it in published UI images. Nothing publishes directly from the feature branch.
