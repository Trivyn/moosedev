
<!--
An example. Copy it to `.moosedev/GUIDANCE.md`, edit it, and commit it.

The paragraph above is the harness's compiled default. A GUIDANCE.md REPLACES
that default rather than adding to it, so keep the paragraph unless you mean to
reword it.

Text before the first `## Plan` or `## Implement` heading reaches both modes.
`## Plan` reaches planning; `## Implement` reaches approved work. Any other
heading is ordinary text, and so is a heading inside a fenced code block.
Comments like this one are stripped before the model sees the file.

Each mode receives the shared text plus its own section; that must fit 4096
bytes, and the whole file 12288.

Hard rules do not belong here. Record them in the project knowledge graph as
Constraints: the harness delivers those as the Project rules block and holds
the plan to them.
-->

## Plan

Name the command that will prove each step, and say which project rule the step
satisfies.

## Implement

Change one file per action, and leave lines you did not come to change alone.
