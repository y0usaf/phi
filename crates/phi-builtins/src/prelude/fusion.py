def _fusion_parse_json(text):
    text = str(text).strip()
    if not text:
        return None
    candidates = [text]
    fenced = _re.search(r"```(?:json)?\s*(\{.*?\})\s*```", text, _re.S)
    if fenced:
        candidates.append(fenced.group(1))
    start = text.find("{")
    end = text.rfind("}")
    if start != -1 and end != -1 and end > start:
        candidates.append(text[start : end + 1])
    for candidate in candidates:
        try:
            value = _json.loads(candidate)
        except Exception:
            continue
        if isinstance(value, dict):
            return value
    return None


def __phi_ext_tool__(name, args=None):
    """Call a Rust extension-registered tool via the subcall protocol."""
    result = _subcall({"kind": "ext_tool", "name": name, "args": args or {}})
    if result.get("error") is not None:
        return "Error: %s" % result["error"]
    return result.get("text", "")


def fusion(
    prompt,
    analysis_models=None,
    judge_model=None,
    perspectives=None,
    recursive=False,
    max_workers=8,
):
    """Run a Fusion-style multi-model / multi-perspective synthesis pass.

    Returns a dict with:
    - panel: list of {model, perspective, response}
    - synthesis: parsed JSON from the judge when possible, else {raw: ...}
    - final_answer: the judge's final answer field (or raw judge output)
    """

    query = rlm_query if recursive else llm_query
    prompt = str(prompt)

    if perspectives is None:
        perspectives = [
            "Direct answer: solve the user's request as straightforwardly as possible.",
            "Skeptical review: stress-test assumptions, edge cases, and possible mistakes.",
            "Implementation focus: emphasize concrete steps, code, commands, or exact evidence.",
            "Concise synthesis: focus on the minimum actionable conclusion and caveats.",
        ]
    elif isinstance(perspectives, str):
        perspectives = [perspectives]
    else:
        perspectives = list(perspectives)
    if not perspectives:
        raise ValueError("fusion(): perspectives must not be empty")

    if analysis_models is None:
        analysis_models = [None] * min(4, len(perspectives))
    elif isinstance(analysis_models, str):
        analysis_models = [analysis_models]
    else:
        analysis_models = list(analysis_models)
    if not analysis_models:
        analysis_models = [None] * min(4, len(perspectives))

    calls = []
    for i, model in enumerate(analysis_models):
        perspective = perspectives[i % len(perspectives)]
        analysis_prompt = (
            "You are one panelist in a Fusion-style deliberation.\n"
            "Perspective: %s\n\n"
            "Task:\n%s\n\n"
            "Respond with:\n"
            "1. Short answer\n"
            "2. Key evidence / reasoning\n"
            "3. Uncertainties / caveats\n"
            "Keep it concise but substantive."
        ) % (perspective, prompt)
        calls.append(
            {
                "model": model,
                "perspective": perspective,
                "prompt": analysis_prompt,
            }
        )

    workers = max(1, min(int(max_workers), len(calls)))
    if len(calls) == 1:
        responses = [query(calls[0]["prompt"], model=calls[0]["model"])]
    else:
        with _ThreadPoolExecutor(max_workers=workers) as pool:
            responses = list(
                pool.map(
                    lambda item: query(item["prompt"], model=item["model"]),
                    calls,
                )
            )

    panel = []
    for item, response in zip(calls, responses):
        panel.append(
            {
                "model": item["model"] or "default",
                "perspective": item["perspective"],
                "response": response,
            }
        )

    judge_prompt = (
        "You are the judge in a Fusion-style deliberation.\n"
        "Synthesize the panelists' answers into a single result.\n\n"
        "Original task:\n%s\n\n"
        "Return a JSON object with exactly these keys:\n"
        "- consensus: list[str]\n"
        "- contradictions: list[str]\n"
        "- partial_coverage: list[str]\n"
        "- unique_insights: list[str]\n"
        "- blind_spots: list[str]\n"
        "- final_answer: str\n\n"
        "Panel responses:\n"
    ) % prompt

    for i, item in enumerate(panel, 1):
        judge_prompt += (
            "\nPanelist %d\n"
            "Model: %s\n"
            "Perspective: %s\n"
            "Response:\n%s\n"
        ) % (i, item["model"], item["perspective"], item["response"])

    judge_text = query(judge_prompt, model=judge_model)
    synthesis = _fusion_parse_json(judge_text)
    final_answer = judge_text
    if synthesis is None:
        synthesis = {"raw": judge_text}
    else:
        final_answer = synthesis.get("final_answer", judge_text)

    return {
        "panel": panel,
        "judge_model": judge_model or "default",
        "synthesis": synthesis,
        "final_answer": final_answer,
    }
