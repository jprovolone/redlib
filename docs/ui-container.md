# UI container image

Pushes to the `ui/ios-reddit-redesign` branch publish a container image to GitHub Container Registry via `.github/workflows/ui-ghcr.yml`. No other branch publishes, and there is no manual trigger.

Image: `ghcr.io/jprovolone/redlib:ui` (also tagged `sha-<short commit>`). Platform: `linux/amd64` only.

```sh
docker pull ghcr.io/jprovolone/redlib:ui
docker run --rm -p 127.0.0.1:8080:8080 ghcr.io/jprovolone/redlib:ui
```

Then open <http://localhost:8080>.

## Why `Dockerfile.ui`

The existing `Dockerfile` downloads an upstream release binary, so it would not contain this branch's changes. `Dockerfile.ui` compiles the checked-out source (`cargo build --locked --release`) in a `rust:1-bookworm` builder (current stable Rust). The apt packages `cmake clang libclang-dev pkg-config build-essential perl` are needed to build BoringSSL for `wreq`. The runtime is `debian:bookworm-slim` with `ca-certificates` and `wget` (healthcheck), running as a non-root user.

## Local preview (before pushing)

```sh
docker build -f Dockerfile.ui -t redlib-ui:local .
docker run -d --name redlib-ui-preview -p 127.0.0.1:8080:8080 redlib-ui:local
docker logs -f redlib-ui-preview
```

Open <http://localhost:8080>. Stop and remove the preview with
`docker rm -f redlib-ui-preview`. Nothing is published by these commands.
The preview binds to localhost only; live feeds still require access to Reddit.
Templates and styles are embedded at compile time, so rebuild the image after changes.

## Appearance and inline videos

- Settings → Appearance → Theme supports System, Light, Dark and Black, plus the existing themes.
- In card/clean feeds, videos autoplay muted when at least half-visible; only one autoplays at a time. Use the bottom-right speaker button to toggle sound. Native video controls remain available.
- Settings → Autoplay videos disables autoplay. HLS is enabled by default so Reddit videos can include audio; disabling HLS may leave the MP4 fallback without audio.
- Autoplay is suppressed for blurred content, reduced-motion preferences and data-saver connections. Compact layout retains thumbnails.

## Notes

- The workflow uses only the automatic `GITHUB_TOKEN`; no secrets are added.
- The first publish creates the package as private. To pull without login, set the package visibility to public in the GitHub package settings.
- Build layers are cached with the GitHub Actions cache.
