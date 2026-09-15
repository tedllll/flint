"""结构化输出从 Python 侧的真实调用（不是 stub）。

跑法：
    $env:DEEPSEEK_API_KEY = [Environment]::GetEnvironmentVariable("DEEPSEEK_API_KEY","User")
    python ask_schema.py

三件事会被验证，都是"做不到就会很难受"的那种：
  1. ask_json 直接返回 dict —— 不用自己去 message.completed 里 parse，
     而且这份 dict 是 flint 本地按 schema 校验过的。
  2. 一个**内容合法但形状不合**的 schema 不会悄悄通过：schema 要求
     `{"date": ...}`，而模型很可能给它习惯的字段名，这时要么被修复轮救回来
     （attempts=2/3），要么抛 SchemaError —— 不会返回一个形状不对的 dict。
  3. schema 写进了会话文件，`--continue` 不带 --schema 也仍然被同一个形状约束。
"""

import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))

from flint_call import SchemaError, ask, ask_json  # noqa: E402

HOME = os.environ.get("FLINT_HOME_DEMO") or None

SCHEMA = {
    "type": "object",
    "properties": {
        "instrument": {"type": "string", "description": "合约代码"},
        "last_trading_day": {"type": "string", "description": "YYYY-MM-DD"},
        "confidence": {"type": "string", "enum": ["high", "low"]},
    },
    "required": ["instrument", "last_trading_day", "confidence"],
    "additionalProperties": False,
}


def main():
    print("1. ask_json：直接拿到被校验过的对象")
    answer = ask_json(
        "MA2610 这个期货合约的代码和它的最后交易日是哪天？如果无法确定日期就说明。",
        cwd=HERE,
        home=HOME,
        schema=SCHEMA,
    )
    print("   ->", json.dumps(answer, ensure_ascii=False))
    assert set(answer) <= set(SCHEMA["properties"]), "flint 之外的东西跑进来了"
    assert answer["instrument"] and answer["last_trading_day"] and answer["confidence"]

    print("\n2. 同一个会话接着问，不重复传 schema")
    turn = ask("再确认一次：这个日期是按什么规则推的？一句话。", cwd=HERE, home=HOME,
               continue_last=True)
    print("   attempts:", turn.attempts, "| keys:", sorted((turn.result or {}).keys()))
    assert turn.result, "会话文件里的形状没有被沿用"
    assert turn.attempts is not None

    print("\n3. 形状不可能满足时，抛异常而不是返回个形状不对的 dict")
    # minLength 5 与 maxLength 1 同时成立是不可能的字符串：无论模型多努力，
    # flint 的本地校验都会拒绝，于是三次之后抛 SchemaError。
    impossible = {
        "type": "object",
        "properties": {"a": {"type": "string", "minLength": 5, "maxLength": 1}},
        "required": ["a"],
        "additionalProperties": False,
    }
    try:
        ask_json("随便说一句话。", cwd=HERE, home=HOME, schema=impossible, timeout=300)
    except SchemaError as e:
        print("   抛了 SchemaError（这是对的）:", str(e).splitlines()[0][:80], "...")
        print("   attempts:", e.attempts)
        assert e.attempts == 3, "没试满三次就放弃了"
    else:
        raise AssertionError("一个不可能满足的 schema 居然通过了")

    print("\n4. --schema 与 --no-schema 同时给出会被拒绝，而不是随便挑一个执行")
    both = ask("说一句话。", cwd=HERE, home=HOME, schema=SCHEMA, no_schema=True)
    print("   exit:", both.returncode, "| result:", both.result)
    assert both.returncode != 0 and both.result is None

    print("\n全部通过")


if __name__ == "__main__":
    main()
