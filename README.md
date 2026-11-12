# OpenIDEA Latte Plugin ☕

Latte template engine support for [OpenIDEA](https://github.com/l0ng-ai/openidea).

Latte is the template engine of the Nette Framework for PHP, known for its strict context-sensitive escaping, intuitive tag syntax, and powerful `n:` attributes.

## Features

- **File detection**: Automatic association for `.latte` files.
- **Syntax highlighting**:
  - Latte tags: `{if ...}`, `{foreach ...}`, `{block ...}`, `{var ...}`, `{dump ...}`, and more.
  - Latte modifiers / filters: `|upper`, `|truncate`, `|date`, `|noescape`, `|stripHtml`, etc.
  - Variables (`$user`, `$iterator`, `$this`) and PHP expressions.
  - HTML `n:` attributes: `n:if`, `n:foreach`, `n:class`, `n:href`, `n:snippet`, etc. with inner expression highlighting.
  - Comments: Latte comments `{* ... *}` and HTML comments `<!-- ... -->`.
  - HTML tags and attributes.
- **Structure View / Outline**:
  - Displays `{block ...}`, `{define ...}`, `{snippet ...}`, `{snippetArea ...}`, and `{layout ...}` in the Structure view.
  - Recognizes `n:block` and `n:snippet` attributes on HTML tags.
  - Displays nested blocks in an interactive hierarchy.
- **Navigation (Go to Definition / ⌘-click)**:
  - Template files in `{include 'file.latte'}`, `{layout}`, `{extends}`, `{import}`, `{embed}`, `{sandbox}` and `{include block from 'file.latte'}` — relative to the template, with a project-wide fallback.
  - Blocks in `{include name}`, `{include #name}`, `{include block name}` and `{ifset #name}` — looked up in the template, its `{import}`s and its layout chain.
  - Gutter icons: jump to the layout / imported templates, and from a `{block}` to the block of the layout it overrides.
  - Needs the optional `project.read` capability for other files; without it, only blocks of the current template resolve.
- **Nette navigation** (following `Presenter::formatTemplateFiles()` / `formatLayoutTemplateFiles()` of Nette 3.2 — templates next to presenters as well as `templates/<Presenter>/<view>.latte` and `templates/<Presenter>.<view>.latte`):
  - `{link}`, `{plink}` and `n:href` → `action<X>()` / `render<X>()` of the presenter (`Product:show`, `:Admin:Product:`, `edit`), signals `logout!` → `handle<X>()`, also inherited from a base presenter.
  - `{control}`, `{form}` and `<form n:name>` → `createComponent<X>()` of the presenter (or of a PHP class next to a component template).
  - Gutter icon on line 1 of a view template → its `render<X>()` / `action<X>()`; in a presenter, gutter icons on `render<X>()` / `action<X>()` → the template.
  - The automatic `@layout.latte` of a presenter template counts as its layout (blocks overriding it get a gutter icon, `{include}` finds its blocks).
- **Code Folding**:
  - Fold block tags (`{block}...{/block}`, `{snippet}...{/snippet}`, `{if}...{/if}`, etc.).
  - Fold multi-line comments (`{* ... *}`, `<!-- ... -->`).
- **File & Plugin Icons**:
  - Custom vector icons for both light and dark themes.
- **Starter Template**:
  - Built-in file template for new `.latte` files.
- **Safety**:
  - Runs in a sandboxed WebAssembly component (`wasm32-wasip2`). Requires 0 dangerous capabilities or system permissions.

## Installation

### Method 1: Install from disk in OpenIDEA
1. Open OpenIDEA.
2. Go to **Settings › Plugins**.
3. Click the gear icon / menu and select **Install Plugin from Disk…**.
4. Choose this directory (`latte/`).

### Method 2: Install from `.oiplugin` package file
1. Run `./build.sh` to generate `community.latte-0.1.0.oiplugin`.
2. In OpenIDEA **Settings › Plugins**, choose **Install Plugin from Package File…** and select the `.oiplugin` file.

### Method 3: Development link
Using OpenIDEA CLI:
```sh
openidea-plugin link .
```

## Building from source

Requirements:
- Rust 1.85+ with `wasm32-wasip2` target:
  ```sh
  rustup target add wasm32-wasip2
  ```

Build and test:
```sh
cargo test
cargo build --release --target wasm32-wasip2
cp target/wasm32-wasip2/release/openidea_plugin_latte.wasm dist/plugin.wasm
```
Or run:
```sh
./build.sh
```

## Publishing to OpenIDEA Community Registry

When ready to publish to [l0ng-ai/openidea-plugins](https://github.com/l0ng-ai/openidea-plugins):
1. Sign the package with your publisher key:
   ```sh
   openidea-plugin pack . --key your_publisher.key --publisher your_publisher
   ```
2. Upload the `.oiplugin` file to a GitHub Release.
3. Submit a PR to `l0ng-ai/openidea-plugins` registering your publisher and plugin version.
