# Local deployment

Build on your machine and upload the packaged frontend and Bun relay to Vercel.
GitHub Actions only runs tests. Automatic Vercel Git deployments are disabled in
`vercel.json`.

## Setup

Install the tools in `mise.toml`, plus Docker and jq, then run from the repo root:

```sh
mise install
npm ci
vc login
vc link --project emulate-computer --scope reb-labs
```

`vc` is the Vercel CLI; `vercel` works too. Activate mise before running commands.
The relay needs `REDIS_URL` configured in Vercel for each deployment environment.
Without it, the site loads but networking returns “Networking unavailable”.
Connect a public Vercel Blob store to the project for guest downloads. `vc pull`
provides its `BLOB_READ_WRITE_TOKEN` locally; it is never included in the frontend.
The project uses the `emulate-images` store with a separate `guest/` prefix.

## Build

For the first build, or after guest changes:

```sh
just build-all
just check
```

For emulator or frontend changes with an existing guest disk and snapshot:

```sh
just build
just check
```

See [build stages](../scripts/README.md) for individual commands. If desktop
source versions or patches change, run `just guest-prebuilt` before `just build-all`.

## Preview

```sh
vc pull --yes --environment=preview
just site-publish preview
vc build --standalone --target=preview
vc deploy --prebuilt --archive=tgz --target=preview
```

`vc build` rebuilds the frontend and packages the relay using the prepared guest
assets and Wasm. `--standalone` includes function dependencies in the output.
`--prebuilt` uploads `.vercel/output` without rebuilding on Vercel.

Check the returned deployment target with `vc inspect <url>`. Vercel may promote
an empty project's first deployment to production even with a preview target;
keep an existing preview when cleaning up this project.

Verify the console and desktop, same-origin networking, and HTTP cache headers
before deploying production. `vc curl / --deployment <url> -- --head` works with
protected previews. Networking requires a working Redis connection, not just a
successful function build.

## Production

Build again with production settings; do not reuse the preview package:

```sh
vc pull --yes --environment=production
just site-publish production
vc build --standalone --target=production
vc deploy --prebuilt --archive=tgz --target=production
```

`site-publish` validates the guest assets, uploads the compressed rootfs and
snapshot to public Blob URLs, and updates the generated manifest. Existing
content-hashed blobs are reused without overwriting them. They have a one-year
cache lifetime and are fetched directly by the browser, without a relay/function
proxy. Blob still charges for transfer, storage, and operations.

The deployment contains the smaller boot files, Wasm, frontend assets, and relay.
The rootfs and snapshot are excluded from its static output. HTML revalidates;
hashed static assets have immutable caching. Keep old blobs while deployed
versions still reference them; publishing does not delete existing blobs.

Run `site-publish` after `just build` and before `vc build` for every deployment.
`just build`, `just web`, or `just site-prepare` restores local guest URLs for
local development; `just site` preserves whichever manifest was prepared.

Node is pinned by mise for tools and tests. Do not add `engines.node` to the root
package manifest: Vercel gives it precedence over `bunVersion`, which would package
the relay for Node instead of Bun. TypeScript rewrites relative `.ts` imports to
`.js` during function packaging so the emitted relay can resolve its dependencies.
