Tarea: decidir si el correo es una oportunidad de venta (alguien interesado en contratar o comprar), una solicitud de soporte (alguien que ya es cliente y pide ayuda o reporta un problema), ambas o ninguna. Da tu confianza entre 0 y 1 y una razón breve en español. Si es oportunidad, propone los datos del lead; si es soporte, los del ticket. Usa los nombres de campo exactos de Iurefficient.

Esquema:
{"category": "sales_opportunity" | "support_request" | "both" | "neither",
 "confidence": 0.0,
 "reason": "una frase",
 "lead": {"name": "nombre completo o empresa", "first_name": "… o null", "last_name": "… o null", "company_name": "… o null", "email": "… o null", "phone": "… o null", "position": "cargo o null", "notes": "qué le interesa"} | null,
 "ticket": {"title": "resumen corto del problema", "description": "detalle", "priority": "low" | "medium" | "high" | "urgent", "category": "… o null", "requester_name": "… o null", "requester_email": "… o null", "tags": ["…"]} | null}
