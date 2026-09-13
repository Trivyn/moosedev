from accounts import SEGMENTS


def statement_line(account, invoice, today, policy):
    return f"{invoice.invoice_id} {SEGMENTS[account.segment]} late fee {policy.late_fee(account, invoice, today)}"
