# Project memory (MOOSEDev)

This project has a **moosedev** memory tool exposing recorded project knowledge — architectural
decisions, lessons, constraints, requirements, and patterns — captured by the maintainers.

When a question asks *why* something is the way it is, or about a past decision, constraint, or
lesson, **consult project memory before answering**: call `get_relevant_context` to recall recorded
entries (use `sparql` for exact reads, and `query` for genuine multi-step questions). Prefer answers
grounded in recorded knowledge over guessing, and cite the recorded entry (its title) you relied on.
