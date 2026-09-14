from decimal import Decimal, ROUND_HALF_UP

GRACE_DAYS = 14
LATE_RATE = Decimal("0.05")
MINIMUM_LATE_FEE = 100
NON_PROFIT_SEGMENTS = {"charity", "foundation"}
NP9_FIRST_DUE_DAY = 1000
NP9_LATE_FEE = 200
RETURNED_PAYMENT_RATE = Decimal("0.025")
MINIMUM_RETURNED_PAYMENT_FEE = 250


def percent_of(amount_cents, rate):
    return int((Decimal(amount_cents) * rate).quantize(Decimal("1"), rounding=ROUND_HALF_UP))


class FeePolicy:
    def late_fee(self, account, invoice, today):
        if today - invoice.due_day <= GRACE_DAYS:
            return 0
        if account.segment in NON_PROFIT_SEGMENTS:
            return NP9_LATE_FEE if invoice.due_day >= NP9_FIRST_DUE_DAY else 0
        return max(MINIMUM_LATE_FEE, percent_of(invoice.amount_cents, LATE_RATE))

    def returned_payment_fee(self, account, amount_cents):
        if account.segment in NON_PROFIT_SEGMENTS:
            return 0
        return max(MINIMUM_RETURNED_PAYMENT_FEE, round(amount_cents * 25 / 1000))
