# Supplemental registry logos

Original SVGs from [Homarr Labs Dashboard Icons](https://github.com/homarr-labs/dashboard-icons),
under Apache-2.0 (`LICENSE`). `sources.json` pins the upstream revision and each
file's SHA-256. Logos and names remain their respective owners' trademarks.

Docker Hub is the primary source. These package assets supplement exact public
repositories hosted elsewhere or missing a Hub logo; matching is defined in
`ui/src/image-brands.ts`. Container names and private mirrors are never guessed.
Unknown images retain the generic icon. The package script copies these fixed
inputs; runtime only reads them, so the plugin package and host bundle stay
unchanged. They are source-controlled inputs, not shared worktree caches.
