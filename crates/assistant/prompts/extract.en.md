Task: analyze the email. Today is {{today}} (time zone {{time_zone}}); turn relative dates ("on Tuesday", "tomorrow at 10") into absolute dates. Write the texts in English.

Schema:
{"intent": "support_request" | "sales_inquiry" | "scheduling" | "legal_matter" | "information" | "other",
 "summary": "one sentence",
 "events": [{"title": "…", "start": "YYYY-MM-DD or YYYY-MM-DDTHH:MM", "end": "same or null", "all_day": true | false, "location": "… or null", "source_text": "original phrase"}],
 "tasks": [{"title": "commitment", "due_date": "YYYY-MM-DD or null", "assignee_hint": "who or null", "source_text": "original phrase"}],
 "amounts": [{"text": "$12,000", "value": 12000, "currency": "MXN | USD | EUR | … or null"}],
 "people": [{"name": "…", "email": "… or null", "phone": "… or null", "organization": "… or null", "role": "… or null"}],
 "organizations": ["…"],
 "sentiment": "positive" | "neutral" | "negative",
 "urgency": "low" | "normal" | "high" | "critical"}
