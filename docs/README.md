# LEAD Docs

Documentation for LEAD (forked from [Zed](https://github.com/zed-industries/zed)).

Much of this tree still describes upstream Zed behavior and links to https://zed.dev/docs — treat product-name mentions carefully when editing.

To preview the docs locally you will need to install [mdBook](https://rust-lang.github.io/mdBook/) (`cargo install mdbook@0.4.40`), generate the action metadata, and then serve:

```sh
script/generate-action-metadata
mdbook serve docs
```

The first command dumps an action manifest to `crates/docs_preprocessor/actions.json`. Without it, the preprocessor cannot validate keybinding and action references in the docs and will report errors. You only need to re-run it when actions change.

It is important to note the version number above. For an unknown reason, as of 2025-04-23, running 0.4.48 will cause odd URL behavior that breaks things.
