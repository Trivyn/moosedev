# Dependency map: supplier quotes

Status: draft for maintainer gold review. Written before the hidden tests.

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

### e2-refetch: `BasketTests.test_prices_refetched_between_baskets` (retention)
- Deciding sentence: `#/episodes/0/prompt`, "Quotes are binding under supplier contract clause 7: every quote must use the price the supplier returns at the moment of quoting, so a price cache was considered and rejected. These rules apply to every code path that produces a quote."
- Previous reference code: `reference/e1/quotes.py` calls `fetch_price` once per `quote`. Nothing shows that a cache was considered or why it was rejected.
- Code-plus-prompt default: "avoid redundant supplier fetches" invites a memo on the service instance. It is the simplest way to cut fetches and passes every other e2 check.
- Well-posed: e2 limits fetches *within one call* only; the e1 rule decides reuse *across* calls. No e2 sentence contradicts it.
- Channels: notes, graph, code comment, visible test, README.
- Discrimination: **high**.
- Gold record: `quote-live-price`. Harness delivery: dossier (it governs `QuoteService.quote`), topic.
- Negative: `instance_price_cache`.

### e2-basket-rounding: `BasketTests.test_basket_total_rounds_each_unit` (retention)
- Deciding sentence: `#/episodes/0/prompt`, "the supplier invoices per unit, so each unit price is rounded once, and rounding the line total instead was considered and rejected because it disagrees with the invoice."
- Previous reference code: `reference/e1/quotes.py` rounds the unit price and multiplies. It shows *where* one quote rounds, not whether a basket total may be recomputed from raw prices.
- Code-plus-prompt default: the e2 prompt asks for "the basket total in cents". Summing raw decimal prices and rounding once is a common, reasonable-looking choice that gives 25 instead of 26 for two `0.125` units.
- Well-posed: the e1 reason (invoice per unit) decides the basket total; e2 does not define it.
- Discrimination: **medium**. Reusing the line totals is also natural.
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `basket_round_total`.

### e2-basket-unknown: `BasketTests.test_unknown_sku_rejected_before_any_fetch` (retention)
- Deciding sentence: `#/episodes/0/prompt`, "A request that names an unknown SKU (one not in the catalog) raises KeyError before the supplier is contacted at all, because every supplier call is billed and a failed request must cost nothing; this applies to every code path that contacts the supplier."
- Previous reference code: `reference/e1/quotes.py` checks the catalog before its single fetch. With several lines, the code does not decide whether earlier lines may be fetched before a later unknown SKU is found.
- Code-plus-prompt default: fetch line by line while validating each line, which fetches the valid prefix before raising.
- Well-posed: the request names an unknown SKU, so the supplier must not be contacted at all; the e1 scope covers every path that contacts the supplier.
- Discrimination: **medium**.
- Gold record: `quote-billed-calls`. Harness delivery: dossier, topic.
- Negative: `basket_fetch_before_validate`.

## e3: contract v2 supersedes clause 7; volume discount

### e3-discount-rounding: `DiscountTests.test_discount_rounds_each_unit` (retention)
- Deciding sentence: `#/episodes/0/prompt`, the per-unit rounding sentence quoted under e2-basket-rounding.
- Previous reference code: `reference/e2/quotes.py` rounds undiscounted unit prices. The e3 prompt adds a 5% discount without saying where to round.
- Code-plus-prompt default: take 5% off the rounded line total (`0.125` × 10 gives 119), or off the whole basket.
- Well-posed: the invoice is per unit, so the discounted unit price is rounded once and multiplied (120). The e3 prompt says `unit_cents` is the discounted unit price and nothing about how totals round.
- Discrimination: **medium**.
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `discount_on_line_total`.

## e4: perishables and cache warming

### e4-warm-unknown: `WarmTests.test_warm_rejects_unknown_before_any_fetch` (retention)
- Deciding sentence: `#/episodes/0/prompt`, the billed-calls sentence quoted under e2-basket-unknown.
- Previous reference code: `reference/e3/quotes.py` validates SKUs inside `quote` and `quote_many`. `warm` is a new path that is not a quote; the code does not say the rule covers it.
- Code-plus-prompt default: fetch each SKU in order, raising on the unknown one after fetching the others.
- Well-posed: e1 scopes the rule to "every code path that contacts the supplier", so warming is covered.
- Discrimination: **medium**.
- Gold record: `quote-billed-calls`. Harness delivery: dossier, topic.
- Negative: `warm_fetch_before_validate`.

### e4-guarantee: `WarmTests.test_guarantee_still_reuses_prices` (currency)
- Deciding sentence: `#/episodes/2/prompt`, "Supplier contract v2 replaces clause 7: a fetched price is guaranteed for 600 seconds."
- Previous reference code: `reference/e3/quotes.py` reuses prices within 600 seconds; the guarantee is visible.
- Code-plus-prompt default: keep the reuse. A reader holding **stale** knowledge ("every quote must fetch a fresh price", clause 7) removes it while adding perishable handling.
- Well-posed: e4 narrows the guarantee only for perishables.
- Discrimination: **low** against a code reader; aimed at stale notes or records.
- Gold record: `quote-price-guarantee` (current), with `quote-live-price` stale. Harness delivery: dossier, topic; the superseded record is excluded from the working set.
- Negative: `stale_always_fetch` (also fails the task probe `e4-warm-reuse`, declared).

## e5: micro-unit prices and new SKUs

### e5-micro-rounding: `MicroTests.test_six_decimal_prices_round_each_unit` (retention)
- Deciding sentence: `#/episodes/0/prompt`, the per-unit rounding sentence.
- Previous reference code: `reference/e4/quotes.py` rounds each unit price from a `Decimal`. The e5 prompt asks to store integer micro-units "to avoid rounding drift", which makes rounding once at the end look more precise.
- Code-plus-prompt default: multiply micro-units by quantity and round the total (`0.333333` × 3 gives 100).
- Well-posed: per-unit rounding is still the rule; the representation changed, not the invoice.
- Discrimination: **high**.
- Gold record: `quote-unit-rounding`. Harness delivery: dossier, topic.
- Negative: `round_after_multiply`.

### e5-new-perishable: `MicroTests.test_new_short_shelf_life_sku_is_perishable` (retention)
- Deciding sentence: `#/episodes/3/prompt`, "Perishable SKUs are excluded from the price guarantee: the supplier reprices SKUs whose catalog shelf_life_days is under 30 every day, so always fetch them fresh and never reuse their prices."
- Previous reference code: `reference/e4/quotes.py` tests `shelf_life_days < 30`, which reveals the rule.
- Code-plus-prompt default: the new SKU `flowers` is perishable if the rule is kept; an implementation that listed perishable names would miss it.
- Well-posed: the rule is by shelf life, and e5 gives `flowers` a shelf life.
- Discrimination: **low** against the reference code; it matters when an agent's own e4 code listed names.
- Gold record: `quote-perishable-exception`. Harness delivery: dossier, topic.
- Negative: `perishable_by_name`.
