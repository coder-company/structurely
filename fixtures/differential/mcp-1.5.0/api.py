@app.get("/health")
def health_check():
    return {"ok": True}
