# **Nova 系统重构需求规格说明书 (Requirements)**

## **1\. 背景与痛点 (Context & Pain Points)**

当前 Nova 系统集成了丰富的底层策略（16 种 Claude Code 策略：Tasks, Plan, Team, Coordinator 等），但在实际运行中遭遇了严重的“落地瓶颈”：

1. **主流程阻塞 (Head-of-Line Blocking)：** LLM 被迫在同步对话流中处理记忆摘要、任务状态更新等重负荷计算，导致响应延迟高，用户体验割裂。  
2. **上下文污染 (Context Bloat)：** 底层工具（如 bash, read\_file）的冗长输出，以及 \<nova\_os\> 状态标签强行注入，严重消耗 Token 预算并引发模型幻觉。  
3. **策略孤岛 (Strategy Silos)：** 众多优秀的策略缺乏统一的高层编排，LLM 陷入“拿着锤子找钉子”的微操狂热，无法优雅处理复杂的架构级任务。  
4. **长期记忆断档 (Memory Amnesia)：** 记忆提取机制（Dream/Compact）触发条件苛刻或严重影响性能，导致高价值知识流失。  
5. **提示词碎片化与纪律失控 (Prompt Fragmentation)：** 核心指导（如记忆法则 MEMORY\_GUIDANCE、技能提取 SKILLS\_GUIDANCE）散落在各个 Rust 模块或外部文件中，极易被用户级的动态上下文（如外部 AGENTS.md）覆盖或污染，导致大模型底层行为规范不可预测。

## **2\. 核心重构目标 (Core Objectives)**

本期重构旨在实现 **“控制面（LLM 的逻辑推理）与数据/执行面（Rust 的高并发运行时）的彻底解耦”**，并重塑**中央提示词架构**：

* 目标 A![][image1]  
  ： 确保主 Agent（Query Loop）保持“秒回”状态，彻底免除状态同步与记忆沉淀的计算负担。  
* 目标 B![][image2]  
  ： 废除 \<nova\_os\> 等代偿机制，主 LLM 上下文仅保留当前对话与宏观蓝图。  
* 目标 C![][image3]  
  ： 利用 Tasks.md 作为 Write-Ahead Log (WAL)，实现系统崩溃重启后的无缝业务接续 (BCP)。  
* 目标 D![][image4]  
  ： 基于自然话题生命周期，静默提取长期记忆，并在话题切换时精准召回。  
* 目标 E![][image5]  
  ： 建立中央 PromptBuilder，严格划清“系统内置硬性纪律”与“用户态动态上下文”的物理边界，确保系统底层运行法则不被篡改。

## **3\. 功能需求 (Functional Requirements)**

### **3.1 状态机拦截与宏观委派**

* **FR-1.1 动态权限控制：** 识别复杂任务意图，动态剥夺主 Agent 的低级工具（bash, read）权限。  
* **FR-1.2 宏观工具注入：** 提供 delegate\_complex\_project 工具，允许主 Agent 将项目一键移交后台。

### **3.2 影子事件总线 (Shadow Event Bus)**

* **FR-2.1 统一通道：** 建立基于 tokio::mpsc 的全异步系统事件通道。  
* **FR-2.2 TaskManager (任务记录官)：** 订阅高频任务进度事件，对 Tasks.md 进行极速、确定性的 CRUD 操作。  
* **FR-2.3 MemoryKeeper (记忆记录官)：** 订阅低频话题结束事件，在闲时唤醒 SideQuery 进行深度知识提炼并写入 MEMORY.md。

### **3.3 动态上下文路由 (Dynamic Context Router)**

* **FR-3.1 旁路滑动窗口分类器：** 基于轻量级 LLM (SLM)，实时分析最近 10 条消息，输出 Continue 或 TopicShift。  
* **FR-3.2 远场召回引擎：** 接收 TopicShift 信号后，触发 AgenticSessionSearch 从历史 JSONL 检索并无缝注入旧有上下文。

### **3.4 中央提示词构建管线 (Central PromptBuilder)**

* **FR-4.1 核心纪律硬编码：** 剥离散落在外部文件中的系统级指令，将其作为静态常量收拢到 Rust 内核（如 prompt\_builder.rs），禁止通过外部 Markdown 随意覆写。  
* **FR-4.2 五层组装管线：** 实现严格的 System Prompt 拼接层级：  
  1. 第一层：基础设定与身份 (Base Identity)  
  2. 第二层：系统执行纪律 (System Enforcements，含防注入与记忆规则)  
  3. 第三层：工具与能力声明 (Tools & Capabilities，基于注册表动态注入限制)  
  4. 第四层：用户上下文与防护 (User Context，如 SOUL.md / USER.md)  
  5. 第五层：动态状态注入 (Dynamic Status)  
* **FR-4.3 截断与防注入屏障 (I/O Shield)：** 加载第四层（用户上下文）时，必须执行严格的字符截断限制（保留头部与尾部核心），并过滤恶意越权指令，防止其破坏第二层建立的核心系统纪律。

## **4\. 非功能需求 (Non-Functional Requirements)**

* **性能：** 旁路分类器与主 LLM 请求必须并发执行，主线程阻塞时间增加为 0。  
* **成本：** 记忆提取（SideQuery）必须采用静默批处理或闲时触发机制，避免无意义的 Token 消耗。  
* **可靠性：** 任务状态必须原子化落盘（Tasks.md），确保掉电或宕机后状态不丢失。

[image1]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAmwAAAA8CAYAAADbhOb7AAAEhUlEQVR4Xu3dO4hcVRgH8Gh8VCo+QNjd2bm7WV1dxGYFQwioWNmLjUYQY6GFWFlYRgtjAiK+wEYLFVRQDNqIxYqChcRCkQTBR3whGokiRgxi/L54Tzh7dnY2IEhGfj/4OOd85547SffnzszOpk0AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAwHqGw+HXUW+3/X8r7nl8ZmbmorZ/qvJ813Vvtf1xFhYWzs1zbR8AYKJFwPm4zJeXl8+u94oMQfPz85eP6H8Q9XjbT21wWlxcPK9ej7Nly5ZBno/a1u5tJM58Njc3d3XbBwCYWBFw3i3z2dnZ7X1vVdjKddlr9Xs7Yryp67rrSmW/XUddWc5F745qvrvMU1z3YNSTdS9e48J6neKaX9tevlbbGwwG18S1r7V9AIDTVr51WJ6mRZBZ6cejUZ/085OBLeY7Rz1dK2L/obaX6nuMEsHqsbjmh5znteWpWgSz20ZVXht1a32PWH9XrwEA/lciMF27tLR0ToakHEvlXhPYxgavWtzz7qitWXmung8Gg6vqa6P3SB3Y6nGU2Pt8RG9NYIvenU3rjGYNADA5SmBr+6WXnwUr84WFhfNXX7VWXPtENV8V+trPx51qYJuenr643zvVwPZMs34+6mjdAwCYGBFkfsvQE/VGLM/K3uzs7HIJTjF/NObbYpzPXoxL2e+/jflK1At1yBqOCWxlXvXWDWzltbKq3rqBreu6l5v+yc+/5fmoXfU+AMDEiCDzV9TKzMzMdB+SMqw9EAHo9agrynXRf7oEp1bdj/kXcW5fH5LyLdF9ZT0YDKaac2MDW3XduMD2U4759C7+jbdU/TXnAQAmTgSZP/pxpR939uOxubm5xRGh58QTuFYbiGJ9JOrF7I97GzX29w7/fWBbc109jyB6WQS5m0sfAGCiRJDZkeOwD2xFCTtd1z0c19zf99Z8Vqyog1KcuS/W35d+BL8Yhj+Xt1Jrce1Lsfdn1IEqlJ0c48z2rKq3YWCL17u0n/+YY7zGp2UfAGCiRKC5oZqvVFu5/raaH496c9M6T9dSHZoiYH3V9svn0KKOlL1+//3h6idsm8uZ+p5Vb8PAVu+t1wMAmDgRavZX8z3N3qEMPe3nz2q5H/Vqe00blmJ9T70e9VZle6bWBrapqalLovdL3avF3uEYNrd9AICJE8HmYDX/vZofjnqvfCEham/ZSxG4bs9+jDd2XXd9vZdi71Db28gGgW3VrxrE+pv8tmrdq3XN75DW/zcAgIky7L9pOaz+VlnM90fg2Vqt8+3LDG0ZqM7seyfexsx5hLa7+v0DMX82xj1Ru+Ieu2P9XNQ7sT42rH63dJT+nm3vYPXaJ8zPz18w/OdPimR9OKLyT5XU6y/zfPx7PqrvDQAwESLIHC5fCoj5U+3+fymf1rW9FP178xufZV1+kQEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABOD38Dm2RVFG/Vn+MAAAAASUVORK5CYII=>

[image2]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAmwAAAA8CAYAAADbhOb7AAAE4UlEQVR4Xu3dS2hdRRgH8PQh1gcqYomWJDcv0IIgbRYiuFIUwYrgRu2iIqjFFwqKoi6suBFaFGptxVJFdFFqFUV0oTsRQVFwoQsFoYK60IIttSrWWr9pZpLp5CTNwkWv/H7wMTPfmTk32f05ufdmYAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWEiv1zs2NDR0fttfjJGRkd1x/vO2n+4Z16ba/mJMTk6ek85HHWqvncyqVasuiHM/t30AgL6WwlFH76J6PTw8fGm9LmLfzraXdN2zFXv2R6jb0PbDksWcn0/X2eh91/YAAE5pEWC2Rz2d69dqfjRfPxb1V64jXSEoicD1XNuLvdvGxsYubvtd0n1j72BXv16Pj4+P1Osi/2wHF1Ojo6O3tecBAPrB8ggyt5dFBJs38jgTmCJQXdYGqCL6W9IYwW08h6plaW/c87O6onc0am9zPJ3fHMPSGP+MOlDVsWp+KK+PpDPxOufG612Z5jG+nW+1PK9PeGIXZ77M06V1HwCgb0SgWR+BakW1fjyPCwa2WO+Ncxt700+uNqaKsHRXrL+P+Xn13rz/73ode26t5puqS8e1r9cq10tgi/Wu4eHhibjXnqjH8s+zodTJ7gcAcMrq5T+B5vlFEXSuy/MFA1vR7BsczX92jPGS2V1zA1isfy+92PtOfS1f73y9Il9fXgJbjE/m/q48XlH2xrXdZQ4A0Fci1Hyc3oMW4/PpKVcdkvJ8WaqYr+0KUNG7J/XjHo+k9cTExHAaU1iL/svN3q7zx6JeSPO4x44490ypdK2ed51Pqidsn0TtjPqmXMtn0gcYDs8cAADoFylsRRi6u1p/EfVV9D4YmH5P2YFyLXoXRjB6tKyL3vQb/rflr9Kow94f9b7c6wxc8/VPJs5dFbU/6s28Xp3H66s9+6LezfOZ3wcA4JQXAevMtlekABW1taP/ULO+N0LcA738oYMYb8rjlgh4N+d5+8Rujvq+aU+cfaqu6G3OP9PXZV/0X43Xvj/P0/vnbhmYfho4MDg4eFb9nXJx7ocyBwDoKzkE/dNRqd91LfWerc7/lsfjgS2ZnJw8PdY/lXWEqqtj/XreNyewRe/hqamp06r1nD3JfP0krq2O1/k07YngdmOML6VedV1gAwD+HyL0vNKbDWZbo+5o93TpVYEtny2VPi26Kc3LtdlTs3vbXr0u5uuPjY1dm8b6XjHeF7/LDVHroq7pTX8577oYfznxNABAH4kwczjC1Z48T+Fna6xXpHmMD7b7axGGXmx7tZUrV56dxq7Q1Wu+6qNrT7JAf3/8fB/l+Uxga/bMPPEDAOhLKZDVIacEtnzteGjL9WEvv0+t1st/8lxI7FlTAlXTn/OELQLgnW2lfgl+zf73h4aGzihno97qzX5Rbtnj/4oCAP0rwsyPUeubXgpNO5revhyu5vy3gOi/1/aS9MGAqG9ziEphalGBrV4X+fyapre9Wc/3hO1gvQYA6AtdT6uKFHwisE21/fn0midarRy2nmj7yUj+91JF7HutXleW1IvYt7ZeLyRC4+VtDwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAP5b/wIbTGtwTZ9Q0QAAAABJRU5ErkJggg==>

[image3]: <data:image/png;base64,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>

[image4]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAmwAAAA8CAYAAADbhOb7AAAFl0lEQVR4Xu3dXailVRkH8DMzZpmJ9jEOntl79j4fdYYBUxiwqzDRujMIFTHyapAuIiSpiKhBCi8cMRzEsISBFBFBhIok0AZRL0rBmFDBYiwwTQiyhOhMo3N6nnGtcblm7/loxjkj/H6weNd61lrvuzlXf9593v3OzAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAR7B2MBic1Rer0Wh0oK+lqL8S7d99vRXnvbCvHa+4xsp4PL60r5+IPOfi4uIH+zoAwCkXweSBaLdE++ER2kq2fm+anZ39cJlbG8d727l2T/RfjlD1oXa+ynULCwvnZz/WbJ6fnz+3b7HmH9F293vTtM9Wxfztub+vp6jvHw6HCxPqGQK/Wsdzc3Mb2nkAgFNpTV84WgBqZUiL9loz/lfprtu0adM1g8HgY9nynNG+Vde1ytxy6X852lzd1+1/qd+b2s8b/eej/bRruXdfu6eV81u3bv1AHHeMSkiNsHZr7ZeW5/hzvxcAYFVkOOlr09S1cVyem5u7pIafCDy/GA6HsxHafn2088X8TTMTgmOrBKa7+noqc49Hd10/F9e/O+Ye6eutDJbteHFxcX35zGe0dQCA08bRAlYV6x6KkPbp0l+JgPbZ0n+mWbM/5yIUfbTW/h+TAluEwgfrXL0L185v2LDh7L5WRX13+395Mf5mHpeWls5p90R/T1znjjoGADgdnBEh5fW+GLXlaH/ta7Ozs59YWFgYRv+eUl7bzK/Mz89/KmslcGX7U96J69a9Wcdbtmw5s9ZbZe+Pm1Ke8+BXpDnXHqsYvxXtybbWyvURJG8s/UczfGYt7xB26/b25wYAWDURYK6Ptj0CygP17tk0Gdby2IaZ8Xi8M8Z78u5WnYvaeFoQSzH9dO3n9XNPtGe7lrWH67py3u/Ufq0388sx/0Rfb8W1vh7td6Ny5y6O/82vcGPfzdEunmm+ps25QxsBAFZTBJPn8onN0TEEthTrfjAcDr+Y4Sd/oiOCzs9L/UC010rQ+ns5Tvz5jzawRf/aKQEs9+9oxr9s52r/SLVp6ueK433R7iz9N/LJ0HLdH717BwDAKqpBZ3QMgS1C2pWx7qq2FuNdeSxfha7L8+XPZuSx/6qx6u6wXX08YSv160dvf8V6zGL9XDnuivbb0q9/hwye323XAwCsmghLNxwtsEV9W9NfKe2FmfI/aBl68hgh7NL169d/JOfjvFvKV6SHPcWZTmZgi8980fHsj2vvrP3Y91iMf1b69e/wxkhgAwBOFyV83Vb6EwNbBKpvN/3Pt3MpA1vU52fK/3/lOSMEjWu/XVvVwDYYDD55ooEt+v8ZT3mqM+Yuq/24zhUx/n6392/Rvlf6NbD9cySwAQCngwwo0b7UjA8LbBGEzht1P16bDx5E/ScRgO7O8ajcYavyvDG/ua31MrDlunwV1EkIbNmfeCcv5t5qx3m9CXvrncLDAttoypsWAADecxGY/hjhbKmtZWAbN69mKrUXmiBzQfajvdo8Efq1aHu7PRnYPtPWWhHQtuaa+k7RIwW2mLt8XJ4MbbXrR2//z9nBd4u2/zMX48+Vz/urWmteq3VQ7m36kwLbxM8FAPCeihDy+qYJP2pb31owoR16J2e+47P2y1x9wnJ/3nWL7tqNGzd+vK7JNwhE/Rt1nGLti1G7tY7Lb7rVa72aYTKO+5rac+3+lPVu/Jtmfdte7j7ztro3jve/c4Z3zhnX/0P0/xJ/o6+Uc/y+XQcA8L4WYWdzBL8vxPG6CDo3RejZ3q85GaY9fXoM1sTneirvtPUT7VsQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA4MT9Dwd7nW7hCmE7AAAAAElFTkSuQmCC>

[image5]: <data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAmwAAAA8CAYAAADbhOb7AAAEiUlEQVR4Xu3dPYhdRRQH8I3xAxURC1mz7tu7+3YxyYJaxEL8IIUgCCoRLCQ2fhSCTVADggRF7LQQCxGLiCjRJiiKikQEETSVGrQQgpAIKimCohDxA1zPSeaayeTtZrWQvPD7weHOnJk7u+n+3Hf3ZWICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAFjJzMzM+23v3+i67rERvd9nZ2c3tP1e/MxH216K++baXor+0oje3qhDbT/l/qhdbR8AYCyVcPN926+NCkwpQtmeqC/b/nL7e7H+Q/m5o+rPEftPOi96H9Tz+fn5wcLCwqVlbWkwGNxerwMAjKUIW9dGuPmkmr8V86ejHmlqaXJy8sL63uFweEUdpGJ8S9y/OSv79Tjq+fre6O+P3s66l2ZmZl7ox92x4LamjEcFtjfqeZz5XL8vr3HWTfU6AMBYagLXa/VarQ1MMX8vAtG2HC8sLJxXr5X1+twMT7cNBoNrqt5ndWCL9VtjvrEJbCec0Y+r3u6ovevXr7+o3xP331mNBTYAYLxFqNnXVe+MlZAzrPf02sA0G/K6adOmc8p9T8T14aqWmvHOvE5PT19ezsvA9lJ13sexdv5qA1vMd0TtrtfyGvdfkpXjOHNLmW+L+Tf1/QAAp70IMPdFoHmxTNfG/GBWvafWBqZe9A+1H5WW/glhq32frCtP2KL+6PfkdTWBLX7vDTF/tSuBLeZPxngual+93xM2AGCsRaD5KuruMv42amuOI+QsZtiJ2tXUSYEt9j4b/SNlenZ37KnX9lJ5Rj1+sL63qz4SzTMidO3J8WoCWzlvY1cCW7vezwU2AOCMUT1pO/qXlm34SW1vcXHx3Oj91fZ7bcBqP2rtTgxsGcBW/YRtbm7u5tL7J7Dlx579uKwJbADAGSGfih2sGxGGJpcLYbUIQ49H0Lus2rt2ovxFZ2rC1pvtmX1gi/oupmviujVrNYGt6vWB7awyzyeBT5WPSJeiXol6JsdTU1MXVLcCAJz+IsR8WkJN/31oByLo3LuawBah6vq8NoEt55vzfbas7Pfjvo6fcDywxc+7qrp//38MbCfJ/Z6wAQBjLYLS1RFqfouQtKXpLxvY2oDUBrbaqH79lCvWv+ia71GL+TqBDQBgBRFy7jhFYPu8nud7abk36roMYPkuXF/Zr+d9VWcdiPtfro47qg5stVG/U/TebXs9gQ0AGHvlfynI97t+KqFre/ZPEdiyf/R9sTQ9PX3lKfYuK9YPR73e9iPUvd320qjzovdh2+u1gS3mO+p1AIDTXgSYnzPUREC6a8RaBrisX6J+reZZ6/p9+S5b9up7e8v1e+Wsd/p5nHVD/zOqPfd35fccdV70vs4v241/wwODweDGunJ/9B+q590KT+QAAM5IEYjuiRD0Y9tPowJWLb8+pO2l4XB4cduLsz5qeyn6h9seAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/P/+BgfEbpklFFkwAAAAAElFTkSuQmCC>