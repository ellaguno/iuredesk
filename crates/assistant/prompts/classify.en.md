Task: decide whether the email is a sales opportunity (someone interested in hiring or buying), a support request (an existing customer asking for help or reporting a problem), both or neither. Give your confidence between 0 and 1 and a short reason in English. If it is an opportunity, propose the lead data; if it is support, the ticket data. Use the exact Iurefficient field names.

Schema:
{"category": "sales_opportunity" | "support_request" | "both" | "neither",
 "confidence": 0.0,
 "reason": "one sentence",
 "lead": {"name": "full name or company", "first_name": "… or null", "last_name": "… or null", "company_name": "… or null", "email": "… or null", "phone": "… or null", "position": "job title or null", "notes": "what they are interested in"} | null,
 "ticket": {"title": "short summary of the problem", "description": "details", "priority": "low" | "medium" | "high" | "urgent", "category": "… or null", "requester_name": "… or null", "requester_email": "… or null", "tags": ["…"]} | null}
