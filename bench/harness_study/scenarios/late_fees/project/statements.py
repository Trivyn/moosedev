def statement_line(account, invoice, today, policy):
    return f"{invoice.invoice_id} {account.segment} late fee {policy.late_fee(account, invoice, today)}"
