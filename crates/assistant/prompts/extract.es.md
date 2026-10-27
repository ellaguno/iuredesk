Tarea: analizar el correo. Hoy es {{today}} (zona {{time_zone}}); convierte fechas relativas ("el martes", "mañana a las 10") en fechas absolutas. Escribe los textos en español.

Esquema:
{"intent": "support_request" | "sales_inquiry" | "scheduling" | "legal_matter" | "information" | "other",
 "summary": "una frase",
 "events": [{"title": "…", "start": "AAAA-MM-DD o AAAA-MM-DDTHH:MM", "end": "igual o null", "all_day": true | false, "location": "… o null", "source_text": "frase original"}],
 "tasks": [{"title": "compromiso", "due_date": "AAAA-MM-DD o null", "assignee_hint": "quién o null", "source_text": "frase original"}],
 "amounts": [{"text": "$12,000", "value": 12000, "currency": "MXN | USD | EUR | … o null"}],
 "people": [{"name": "…", "email": "… o null", "phone": "… o null", "organization": "… o null", "role": "… o null"}],
 "organizations": ["…"],
 "sentiment": "positive" | "neutral" | "negative",
 "urgency": "low" | "normal" | "high" | "critical"}
