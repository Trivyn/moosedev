# Project memory (MOOSEDev)

This project has a **moosedev** memory tool exposing recorded project knowledge — architectural
decisions, lessons, constraints, requirements, and patterns — captured by the maintainers.

Consult project memory **before answering a *why* question and before making an implementation
choice** — adding a dependency or library, creating a new component or module, or picking an
approach where the project may already have a convention: call `get_relevant_context` to recall
recorded entries (use `sparql` for exact reads, and `query` for genuine multi-step questions).
Prefer choices and answers grounded in recorded knowledge over guessing, and cite the recorded
entry (its title) you relied on.
