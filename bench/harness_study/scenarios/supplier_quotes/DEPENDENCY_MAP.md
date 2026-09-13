# Dependency map: supplier quotes

Status: draft for maintainer gold review, revision 2 after blind reader audit round 1. Written before the hidden tests.

Each probe records its round-1 verdict. Condition A gave a reader only the episode prompt and the previous reference code; condition B added only the deciding records. INFERABLE means condition A answered correctly and with confidence.

Every retention probe is labelled with what it measures. **Correctness**: the previous code cannot show the rule, so an arm without the knowledge is expected to get it wrong. **Cost**: the rule is visible in the previous code, so a graph-first agent's value is not having to read and infer it from source; it is scored on reads, searches, requests and tokens before the first correct edit, not on pass or fail alone.

Each later probe is decided by a sentence in an earlier episode prompt. The
columns are: the deciding sentence, what the previous reference code reveals,
the default a reader with only that code and the current prompt would pick,
why the probe is well-posed, the legitimate channels, the expected
discrimination, the gold record that decides it, and how the harness delivers
that record.

Harness delivery routes:
- **dossier**: the record is linked to `quotes.py` `QuoteService.quote` (the
  resolution target), so its complete claims arrive whenever the task attaches
  `quotes.py`.
- **topic**: complete claims for the top 12 records matching the objective and
  guidance. This project stays under a dozen records, so every current record
  is expected to arrive this way too.
- **search**: `search(query)` returns matching accepted knowledge first.

Native OpenCode reads `PROJECT_NOTES.md` in full. Code comments, visible tests
and README text written by either arm are legitimate channels in both arms.

## e1: task probes only

| Probe | Test | Decided by |
| --- | --- | --- |
| e1-half-up | `QuoteTests.test_half_up_per_unit` | e1 prompt (`#/episodes/0/prompt`) |
| e1-quantity | `QuoteTests.test_rejects_non_positive_quantity` | e1 prompt |
| e1-unknown | `QuoteTests.test_unknown_sku_rejected_before_fetch` | e1 prompt |
| e1-live | `QuoteTests.test_every_quote_fetches` | e1 prompt |

## e2: batch quoting under a rate limit

### e2-refetch: `BasketTests.test_prices_refetched_between_baskets` (retention, measures correctness, revised)
- Round 1: INFERABLE. The e2 prompt scoped the dedupe rule to "within one quote_many call", which answered the cross-call question.
- Revision: the e2 prompt now says only "avoid redundant supplier fetches".
- Deciding sentence: `#/episodes/0/prompt`, "Quotes are binding under supplier contract clause 7: every quote must use the price the supplier returns at the moment of quoting, so a price cache was considered and rejected. These rules apply to every code path that produces a quote."
- Previous reference code: `reference/e1/quotes.py` calls `fetch_price` once per `quote`. Nothing shows that a cache was considered or why it was rejected.
- Code-plus-prompt default: an unscoped "avoid redundant supplier fetches" invites a memo on the service instance.
- Well-posed: within one call a repeated SKU is quoted at one moment; across calls clause 7 requires a fresh price. No e2 sentence contradicts it.
- Discrimination: **high**.
- Gold record: `quote-live-price`. Harness delivery: dossier (it governs `QuoteService.quote`), topic.
- Negative: `instance_price_cache`.

### e2-basket-rounding: `BasketTests.test_basket_total_rounds_each_unit` (retention, measures cost)
- Round 1: INFERABLE. The per-unit `to_cents` helper and `total_cents = unit_cents * qty` in `reference/e1/quotes.py` show the rule.
- Deciding sentence: `#/episodes/0/prompt`, the per-unit rounding sentence quoted under e3-discount-rounding.
- Cost measured: a graph-first arm takes the rounding rule from `quote-unit-rounding` instead of reading `quotes.py` to infer it.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `basket_round_total`.

### e2-basket-unknown: `BasketTests.test_unknown_sku_rejected_before_any_fetch` (retention, measures correctness)
- Round 1: PASS (condition A answered one supplier call).
- Deciding sentence: `#/episodes/0/prompt`, "A request that names an unknown SKU (one not in the catalog) raises KeyError before the supplier is contacted at all, because every supplier call is billed and a failed request must cost nothing; this applies to every code path that contacts the supplier."
- Previous reference code: `reference/e1/quotes.py` checks the catalog before its single fetch; with several lines it does not decide whether earlier lines may be fetched first.
- Code-plus-prompt default: validate and fetch line by line.
- Discrimination: **medium**.
- Gold record: `quote-billed-calls`. Harness delivery: dossier, topic.
- Negative: `basket_fetch_before_validate`.

## e3: contract v2 supersedes clause 7; volume discount

### e3-discount-rounding: `DiscountTests.test_discount_rounds_each_unit` (retention, measures correctness, revised)
- Round 1: INFERABLE. The prompt said "unit_cents is then the discounted unit price", which fixed per-unit rounding.
- Revision: the discount is stated at the line level, "a line of 10 or more units costs 5% less", and the test prices a line where rounding the discounted line total (119) differs from rounding each discounted unit (120).
- Deciding sentence: `#/episodes/0/prompt`, "the supplier invoices per unit, so each unit price is rounded once, and rounding the line total instead was considered and rejected because it disagrees with the invoice."
- Previous reference code: `reference/e2/quotes.py` rounds undiscounted unit prices; it does not say where a line-level discount is rounded.
- Code-plus-prompt default: take 5% off the line total and round once (119).
- Well-posed: a line costing 5% less means each unit costs 5% less; the invoice is per unit, so each discounted unit price is rounded once.
- Discrimination: **medium** (the per-unit helper is still visible).
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `discount_on_line_total`.

## e4: perishables and cache warming

### e4-warm-unknown: `WarmTests.test_warm_rejects_unknown_before_any_fetch` (retention, measures correctness)
- Round 1: PASS, weak (condition A right but ambiguous).
- Deciding sentence: `#/episodes/0/prompt`, the billed-calls sentence quoted under e2-basket-unknown.
- Previous reference code: `reference/e3/quotes.py` validates inside `quote` and `quote_many`; `warm` is a new path that is not a quote.
- Code-plus-prompt default: fetch each SKU in order, raising on the unknown one after fetching the others.
- Discrimination: **medium**.
- Gold record: `quote-billed-calls`. Harness delivery: dossier, topic.
- Negative: `warm_fetch_before_validate`.

### e4-guarantee: `WarmTests.test_guarantee_still_reuses_prices` (currency)
- Round 1: CURRENCY. A no-memory reader is right by design; condition A does not apply to currency probes.
- Deciding sentence: `#/episodes/2/prompt`, "Supplier contract v2 replaces clause 7: a fetched price is guaranteed for 600 seconds."
- Stale knowledge (clause 7, "every quote must fetch a fresh price") removes the reuse.
- Gold record: `quote-price-guarantee` (current), with `quote-live-price` stale. Harness delivery: dossier, topic; the superseded record is excluded from the working set.
- Negative: `stale_always_fetch` (also fails the task probe `e4-warm-reuse`, declared).

## e5: micro-unit prices, new SKUs and nightly order batches

### e5-orders-unknown: `OrderTests.test_orders_reject_unknown_before_any_fetch` (retention, measures correctness, new)
- Round 1: not audited (new in revision 2).
- Deciding sentence: `#/episodes/0/prompt`, the billed-calls sentence quoted under e2-basket-unknown.
- Previous reference code: `reference/e4/quotes.py` validates within one basket (`quote_many`) and within `warm`. A batch of baskets has no precedent.
- Code-plus-prompt default: return `[self.quote_many(order) for order in orders]`, which fetches the earlier baskets' SKUs before a later basket's unknown SKU raises.
- Well-posed: the whole `quote_orders` call is one request that names an unknown SKU, so the supplier must not be contacted at all.
- Discrimination: **medium to high**.
- Gold record: `quote-billed-calls`. Harness delivery: dossier, topic.
- Negative: `orders_loop`.

### e5-micro-rounding: `MicroTests.test_six_decimal_prices_round_each_unit` (retention, measures cost)
- Round 1: INFERABLE. Per-unit rounding is visible in `reference/e4/quotes.py`.
- Deciding sentence: `#/episodes/0/prompt`, the per-unit rounding sentence. The e5 prompt's "to avoid rounding drift" makes rounding once at the end look more precise.
- Cost measured: taking the rule from `quote-unit-rounding` instead of re-deriving it from the old helper while replacing it.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `round_after_multiply`.

### e5-new-perishable: `MicroTests.test_new_short_shelf_life_sku_is_perishable` (retention, measures cost)
- Round 1: INFERABLE. The shelf-life test in `reference/e4/quotes.py` decides it.
- Deciding sentence: `#/episodes/3/prompt`, "the supplier reprices SKUs whose catalog shelf_life_days is under 30 every day, so always fetch them fresh and never reuse their prices."
- Cost measured: taking perishability by shelf life from `quote-perishable-exception` instead of reading `quotes.py`.
- Discrimination: **low** for correctness; cost probe.
- Gold record: `quote-perishable-exception`. Harness delivery: dossier, topic.
- Negative: `perishable_by_name`.
