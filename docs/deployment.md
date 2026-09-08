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
No GitHub deployment token or asset release setting is required.

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
vc build --standalone --target=production
vc deploy --prebuilt --archive=tgz --target=production
```

The deployment contains content-hashed guest files, Wasm, frontend assets, and
the relay function. HTML revalidates; hashed assets have immutable caching.

Node is pinned by mise for tools and tests. Do not add `engines.node` to the root
package manifest: Vercel gives it precedence over `bunVersion`, which would package
the relay for Node instead of Bun. TypeScript rewrites relative `.ts` imports to
`.js` during function packaging so the emitted relay can resolve its dependencies.
