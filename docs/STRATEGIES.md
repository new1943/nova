# NOVA 策略与配置手册

**版本**: v3.1
**日期**: 2026-04-20

---

## 一、配置参数

| 配置项 | 默认值 | 说明 |
|:---|:---|:---|
| model | MiniMax-M2.7 | 模型名称 |
| context_window | 200000 | 上下文窗口tokens |
| max_turns | 20 | 单次最大轮数 |
| tool_timeout_secs | 60 | 工具超时秒 |
| budget_trigger_pct | 0.9 | 触发阈值90% |
| compact_target_pct | 0.6 | 保留60% |

---

## 二、Token Budget 双阈值

**阈值1 触发Compact**: input_tokens > context_window * 0.9

**阈值2 边际递减**: current_delta > prev_delta * 3.0 且 current_delta > 10000

---

## 三、Compact 双层熔断

| 水位 | 模式 |
|:---|:---|
| 85%-95% | Graceful LLM生成JSON |
| 大于95% | Forceful 直接丢弃 |

---

## 四、话题状态机

Started - Active - Suspended - Archived

切换信号词: 好/搞定/下一个/换个话题/先这样/好了/结束/完成

---

## 五、张力值

公式: tension = intimacy*0.4 + trust*0.2 + emotion_weight + context_weight + intent_boost - gap_penalty

情绪权重: Calm=0, Excited=15, Anxious=25, Frustrated=30, Tense=35

模式: Normal小于50, SoftIntimate 50-75, HighIntimate大于75

---

## 六、Bash安全

始终阻止: rm -rf, mkfs, dd if, shutdown, reboot, halt, poweroff

始终阻止操作符: 命令替换/反引号/进程替换/重定向

---

## 七、IO Shield截断

Bash: 20000字符, 头尾各8000

Browser: 15000字符, 头尾各5000

---

## 八、工具清单

bash, read_file, write_file, glob, grep, file_edit, browser, agentic_search, memory, execute_react, execute_chain, execute_parallel, execute_with_review, execute_project, task_list, task_stop, worktree, skills

---

## 九、文件路径

配置: ~/.nova/config
MEMORY: ~/.nova/MEMORY.md
Daily: ~/.nova/memories/
Sessions: ~/.nova/sessions/
Socket: /tmp/nova.sock
