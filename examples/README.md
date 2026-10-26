# Examples

Each folder is a small, self-contained flow. They call a real model through Rig, so they need
`ANTHROPIC_API_KEY`. Put it in `examples/.env` (ignored by git):

```sh
ANTHROPIC_API_KEY=sk-ant-...
```

Then run one from this directory, exporting the file's variables first (`set -a` exports them;
a plain `source .env` does not, unless each line starts with `export`):

```sh
set -a; source .env; set +a; cargo run -p example-hello-world
```

| Example | Flow |
|---|---|
| [`hello-world`](hello-world) | Smallest agent: one turn, no tools, a real reply from Claude Sonnet 5.5 |
| [`instructions`](instructions) | Templated returned instructions plus a static `use_instruction` style guide, a real reply |
| [`skills`](skills) | Mount a skill embedded at compile time (sample, `use_skill` undecided and ignored for now) |
