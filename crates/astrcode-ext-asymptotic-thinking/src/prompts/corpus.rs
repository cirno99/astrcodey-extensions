//! 27 个领域提示词模块的数据表，由 `tools/port-prompts.mjs` 从上游 TS 生成。
//!
//! **勿手工编辑**：改这张表不会改上游，只会让 `tests/prompts.rs` 的逐字对照失败。
//! 要更新语料，重跑生成器并 review 本文件的 diff。
//!
//! 每个模块的正文按「状态 × 四档难度」拆开存；上游 `buildPrompt` 的 `switch` 正是这个形状，
//! 生成器会断言 `HARD == EXTREME` 且 `MODERATE == COMPLEX`，不成立就直接失败。
//!
//! `understand_tail` 是 DEEP_UNDERSTAND 首行（`当前为…（…难度）。`）之后的固定正文；
//! 首行由 [`super::assemble`] 按模块标签与当前难度重建。

use super::{Module, Tiered};

/// `coding/java-dev`。
pub static CODING_JAVA_DEV: Module = Module {
    label: "编程类Java开发",
    hint: "显式标注泛型类型参数 · 权衡checked/unchecked异常策略",
    understand_tail: "\n\n明确技术栈（纯Java/Spring Boot/Android/其他）及JDK版本\n→ 识别类型层级与泛型约束\n→ 评估异常策略（checked/unchecked）与null安全处理\n\n领域要点：显式标注泛型类型参数 · 权衡checked/unchecked异常策略\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n警惕NPE风险点 · 泛型类型标注完整 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n警惕NPE风险点 · 泛型类型标注完整 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n警惕NPE风险点 · 泛型类型标注完整 · 异常处理策略一致\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无raw type · null安全处理到位 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
        simple: "无raw type · null安全处理到位 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
        standard: "无raw type · null安全处理到位 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无raw type · null安全处理到位 · 异常处理策略一致\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/rust-dev`。
pub static CODING_RUST_DEV: Module = Module {
    label: "编程类Rust开发",
    hint: "所有权与借用检查 · 生命周期标注 · unsafe边界控制",
    understand_tail: "\n\n明确Rust版本与edition → 识别所有权模型与生命周期约束 → 评估unsafe使用必要性\n\n领域要点：所有权与借用检查 · 生命周期标注 · unsafe边界控制\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n所有权转移明确 · 生命周期标注完整 · unsafe块最小化\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n所有权转移明确 · 生命周期标注完整 · unsafe块最小化\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n所有权转移明确 · 生命周期标注完整 · unsafe块最小化\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无clippy警告 · 所有权正确 · unsafe有文档说明\n\n严格遵守《编程与架构准则》",
        simple: "无clippy警告 · 所有权正确 · unsafe有文档说明\n\n严格遵守《编程与架构准则》",
        standard: "无clippy警告 · 所有权正确 · unsafe有文档说明\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无clippy警告 · 所有权正确 · unsafe有文档说明\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/python-dev`。
pub static CODING_PYTHON_DEV: Module = Module {
    label: "编程类Python开发",
    hint: "Pythonic风格 · 类型注解完整 · 虚拟环境与依赖管理",
    understand_tail: "\n\n明确Python版本与虚拟环境 → 识别类型注解覆盖 → 评估依赖兼容性\n\n领域要点：Pythonic风格 · 类型注解完整 · 虚拟环境与依赖管理\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nPEP 8风格一致 · 类型注解完整 · 依赖锁定到版本\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nPEP 8风格一致 · 类型注解完整 · 依赖锁定到版本\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nPEP 8风格一致 · 类型注解完整 · 依赖锁定到版本\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无mypy错误 · 无裸except · 依赖声明完整\n\n严格遵守《编程与架构准则》",
        simple: "无mypy错误 · 无裸except · 依赖声明完整\n\n严格遵守《编程与架构准则》",
        standard: "无mypy错误 · 无裸except · 依赖声明完整\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无mypy错误 · 无裸except · 依赖声明完整\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/js-dev`。
pub static CODING_JS_DEV: Module = Module {
    label: "编程类JavaScript开发",
    hint: "ES模块规范 · TypeScript类型安全 · 异步错误处理",
    understand_tail: "\n\n明确运行时(Node/Deno/Bun/浏览器) → 识别模块系统(ESM/CJS) → 评估异步模式\n\n领域要点：ES模块规范 · TypeScript类型安全 · 异步错误处理\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nasync/await一致 · Promise错误处理完整 · 类型标注准确\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nasync/await一致 · Promise错误处理完整 · 类型标注准确\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\nasync/await一致 · Promise错误处理完整 · 类型标注准确\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无any类型 · 无未处理的Promise · 模块导入路径正确\n\n严格遵守《编程与架构准则》",
        simple: "无any类型 · 无未处理的Promise · 模块导入路径正确\n\n严格遵守《编程与架构准则》",
        standard: "无any类型 · 无未处理的Promise · 模块导入路径正确\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无any类型 · 无未处理的Promise · 模块导入路径正确\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/go-dev`。
pub static CODING_GO_DEV: Module = Module {
    label: "编程类Go开发",
    hint: "显式错误处理 · 接口隔离 · 并发安全",
    understand_tail: "\n\n明确Go版本与模块路径 → 识别接口抽象层级 → 评估并发模型\n\n领域要点：显式错误处理 · 接口隔离 · 并发安全\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n错误不忽略 · 接口小而专注 · goroutine泄漏检查\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n错误不忽略 · 接口小而专注 · goroutine泄漏检查\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n错误不忽略 · 接口小而专注 · goroutine泄漏检查\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无未处理error · 无数据竞争 · defer资源释放完整\n\n严格遵守《编程与架构准则》",
        simple: "无未处理error · 无数据竞争 · defer资源释放完整\n\n严格遵守《编程与架构准则》",
        standard: "无未处理error · 无数据竞争 · defer资源释放完整\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无未处理error · 无数据竞争 · defer资源释放完整\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/crud-dev`。
pub static CODING_CRUD_DEV: Module = Module {
    label: "编程类增删改查",
    hint: "参数校验前置 · SQL注入防护 · 事务边界明确",
    understand_tail: "\n\n明确数据模型与关系 → 识别CRUD操作范围 → 评估并发与幂等性\n\n领域要点：参数校验前置 · SQL注入防护 · 事务边界明确\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n输入校验前置 · 参数化查询 · 事务粒度合理\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n输入校验前置 · 参数化查询 · 事务粒度合理\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n输入校验前置 · 参数化查询 · 事务粒度合理\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "无SQL拼接 · 空值处理完整 · 批量操作有上限\n\n严格遵守《编程与架构准则》",
        simple: "无SQL拼接 · 空值处理完整 · 批量操作有上限\n\n严格遵守《编程与架构准则》",
        standard: "无SQL拼接 · 空值处理完整 · 批量操作有上限\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n无SQL拼接 · 空值处理完整 · 批量操作有上限\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/bug-fix`。
pub static CODING_BUG_FIX: Module = Module {
    label: "编程类缺陷修复",
    hint: "先复现再定位根因，最小改动修复并补回归测试",
    understand_tail: "\n\n复现步骤明确 → 定位根因而非症状 → 评估影响范围\n\n领域要点：先复现再定位根因，最小改动修复并补回归测试\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n最小改动修复 · 补回归测试 · 不引入新问题\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n最小改动修复 · 补回归测试 · 不引入新问题\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n最小改动修复 · 补回归测试 · 不引入新问题\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "原bug复现失败 · 回归测试通过 · 无副作用\n\n严格遵守《编程与架构准则》",
        simple: "原bug复现失败 · 回归测试通过 · 无副作用\n\n严格遵守《编程与架构准则》",
        standard: "原bug复现失败 · 回归测试通过 · 无副作用\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n原bug复现失败 · 回归测试通过 · 无副作用\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/code-refactor`。
pub static CODING_CODE_REFACTOR: Module = Module {
    label: "编程类代码重构",
    hint: "先补测试再重构，小步提交、保持功能等价",
    understand_tail: "\n\n识别重构目标与范围 → 补足测试覆盖 → 评估依赖影响\n\n领域要点：先补测试再重构，小步提交、保持功能等价\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n小步提交 · 每步验证功能等价 · 不混入新功能\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n小步提交 · 每步验证功能等价 · 不混入新功能\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n小步提交 · 每步验证功能等价 · 不混入新功能\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "测试全绿 · 功能等价 · 代码更清晰\n\n严格遵守《编程与架构准则》",
        simple: "测试全绿 · 功能等价 · 代码更清晰\n\n严格遵守《编程与架构准则》",
        standard: "测试全绿 · 功能等价 · 代码更清晰\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n测试全绿 · 功能等价 · 代码更清晰\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/testing`。
pub static CODING_TESTING: Module = Module {
    label: "编程类程序测试",
    hint: "覆盖正常/边界/异常路径，测试独立可重复",
    understand_tail: "\n\n明确测试范围与类型(单元/集成/E2E) → 识别边界与异常路径 → 评估测试数据需求\n\n领域要点：覆盖正常/边界/异常路径，测试独立可重复\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n测试独立无依赖 · 覆盖正常/边界/异常 · Mock外部依赖\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n测试独立无依赖 · 覆盖正常/边界/异常 · Mock外部依赖\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n测试独立无依赖 · 覆盖正常/边界/异常 · Mock外部依赖\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "测试可重复执行 · 覆盖率达标 · 无flaky test\n\n严格遵守《编程与架构准则》",
        simple: "测试可重复执行 · 覆盖率达标 · 无flaky test\n\n严格遵守《编程与架构准则》",
        standard: "测试可重复执行 · 覆盖率达标 · 无flaky test\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n测试可重复执行 · 覆盖率达标 · 无flaky test\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/architect`。
pub static CODING_ARCHITECT: Module = Module {
    label: "编程类架构设计",
    hint: "关注非功能需求，方案对比有据、取舍明确",
    understand_tail: "\n\n明确架构目标与约束 → 识别非功能需求(性能/安全/可维护) → 评估技术选型\n\n领域要点：关注非功能需求，方案对比有据、取舍明确\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n方案对比有据 · 取舍明确 · 架构图清晰\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n方案对比有据 · 取舍明确 · 架构图清晰\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n方案对比有据 · 取舍明确 · 架构图清晰\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "方案满足非功能需求 · 技术选型有依据 · 风险已识别\n\n严格遵守《编程与架构准则》",
        simple: "方案满足非功能需求 · 技术选型有依据 · 风险已识别\n\n严格遵守《编程与架构准则》",
        standard: "方案满足非功能需求 · 技术选型有依据 · 风险已识别\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n方案满足非功能需求 · 技术选型有依据 · 风险已识别\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/code-review`。
pub static CODING_CODE_REVIEW: Module = Module {
    label: "编程类代码审查",
    hint: "对照规范逐项检查，问题分级、建议可执行",
    understand_tail: "\n\n明确审查范围与规范 → 识别高风险变更 → 评估测试覆盖\n\n领域要点：对照规范逐项检查，问题分级、建议可执行\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n问题分级(阻塞/建议/优化) · 建议具体可执行 · 正向反馈\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n问题分级(阻塞/建议/优化) · 建议具体可执行 · 正向反馈\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n问题分级(阻塞/建议/优化) · 建议具体可执行 · 正向反馈\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "阻塞项已解决 · 规范检查通过 · 无遗漏文件\n\n严格遵守《编程与架构准则》",
        simple: "阻塞项已解决 · 规范检查通过 · 无遗漏文件\n\n严格遵守《编程与架构准则》",
        standard: "阻塞项已解决 · 规范检查通过 · 无遗漏文件\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n阻塞项已解决 · 规范检查通过 · 无遗漏文件\n\n严格遵守《编程与架构准则》",
    },
};

/// `coding/perf-optimize`。
pub static CODING_PERF_OPTIMIZE: Module = Module {
    label: "编程类性能优化",
    hint: "先测量再优化，定位瓶颈、验证提升幅度",
    understand_tail: "\n\n明确性能指标与基线 → 定位瓶颈(profiling) → 评估优化空间\n\n领域要点：先测量再优化，定位瓶颈、验证提升幅度\n\n严格遵守《编程与架构准则》",
    design: Tiered {
        trivial: "",
        simple: "确定技术路径和关键步骤 → 列出涉及文件与验收标准。\n\n严格遵守《编程与架构准则》",
        standard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n\n严格遵守《编程与架构准则》",
        hard: "确定技术栈和依赖项 → 设计数据流和模块边界 → 列出涉及文件、前置依赖、核心步骤、验收标准 → 考虑错误处理和边界条件。\n→ 列出多个备选方案并对比优劣。\n\n严格遵守《编程与架构准则》",
    },
    execute: Tiered {
        trivial: "快速完成目标后 asymptotic-think_transition 进入 VERIFY。\n\n严格遵守《编程与架构准则》",
        simple: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n先测量基线 · 单点优化验证 · 不牺牲可读性\n\n严格遵守《编程与架构准则》",
        standard: "错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n先测量基线 · 单点优化验证 · 不牺牲可读性\n\n严格遵守《编程与架构准则》",
        hard: "每步完成后验证结果 → 错误分析根因后修正 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n先测量基线 · 单点优化验证 · 不牺牲可读性\n每步必须有明确验证，不可跳过\n\n严格遵守《编程与架构准则》",
    },
    verify: Tiered {
        trivial: "性能指标提升 · 无功能回退 · 基准测试可复现\n\n严格遵守《编程与架构准则》",
        simple: "性能指标提升 · 无功能回退 · 基准测试可复现\n\n严格遵守《编程与架构准则》",
        standard: "性能指标提升 · 无功能回退 · 基准测试可复现\n\n严格遵守《编程与架构准则》",
        hard: "逐项对照需求和方案检查 → 功能完整 → 边界条件覆盖 → 代码规范 → 性能达标。\n性能指标提升 · 无功能回退 · 基准测试可复现\n\n严格遵守《编程与架构准则》",
    },
};

/// `retrieval/paper-retrieval`。
pub static RETRIEVAL_PAPER_RETRIEVAL: Module = Module {
    label: "检索类论文检索",
    hint: "优先权威学术源，标注发表时间与引用量",
    understand_tail: "\n\n明确检索主题与范围 → 识别关键作者与会议/期刊 → 评估时效性要求\n\n领域要点：优先权威学术源，标注发表时间与引用量",
    design: Tiered {
        trivial: "",
        simple: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        standard: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        hard: "确定检索策略和关键来源 → 列出检索词与筛选标准 → 设计去重与排序规则。\n→ 列出多个检索渠道并对比覆盖范围。",
    },
    execute: Tiered {
        trivial: "快速检索后 asymptotic-think_transition 进入 VERIFY。",
        simple: "多渠道并行检索(arxiv/semantic scholar/Google Scholar) · 标注来源与时间 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "多渠道并行检索(arxiv/semantic scholar/Google Scholar) · 标注来源与时间 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "多渠道并行检索(arxiv/semantic scholar/Google Scholar) · 标注来源与时间 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "来源权威 · 时效符合要求 · 关键论文无遗漏",
        simple: "来源权威 · 时效符合要求 · 关键论文无遗漏",
        standard: "来源权威 · 时效符合要求 · 关键论文无遗漏",
        hard: "逐项对照需求检查 → 来源权威 → 时效符合 → 覆盖完整。来源权威 · 时效符合要求 · 关键论文无遗漏",
    },
};

/// `retrieval/daily-retrieval`。
pub static RETRIEVAL_DAILY_RETRIEVAL: Module = Module {
    label: "检索类日常检索",
    hint: "直接检索，结果去重后提炼关键信息",
    understand_tail: "\n\n明确检索意图与范围 → 识别可信来源 → 评估信息时效性\n\n领域要点：直接检索，结果去重后提炼关键信息",
    design: Tiered {
        trivial: "",
        simple: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        standard: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        hard: "确定检索策略和关键来源 → 列出检索词与筛选标准 → 设计去重与排序规则。\n→ 列出多个检索渠道并对比覆盖范围。",
    },
    execute: Tiered {
        trivial: "快速检索后 asymptotic-think_transition 进入 VERIFY。",
        simple: "多源并行检索 · 结果去重排序 · 提炼关键信息 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "多源并行检索 · 结果去重排序 · 提炼关键信息 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "多源并行检索 · 结果去重排序 · 提炼关键信息 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "信息准确 · 来源可信 · 时效符合要求",
        simple: "信息准确 · 来源可信 · 时效符合要求",
        standard: "信息准确 · 来源可信 · 时效符合要求",
        hard: "逐项对照需求检查 → 信息准确 → 来源可信 → 时效符合。信息准确 · 来源可信 · 时效符合要求",
    },
};

/// `retrieval/doc-retrieval`。
pub static RETRIEVAL_DOC_RETRIEVAL: Module = Module {
    label: "检索类文档检索",
    hint: "优先官方文档与源码，标注版本兼容性",
    understand_tail: "\n\n明确技术栈与版本 → 识别官方文档入口 → 评估社区资源质量\n\n领域要点：优先官方文档与源码，标注版本兼容性",
    design: Tiered {
        trivial: "",
        simple: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        standard: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        hard: "确定检索策略和关键来源 → 列出检索词与筛选标准 → 设计去重与排序规则。\n→ 列出多个检索渠道并对比覆盖范围。",
    },
    execute: Tiered {
        trivial: "快速检索后 asymptotic-think_transition 进入 VERIFY。",
        simple: "优先官方文档 · 标注版本号 · 交叉验证社区方案 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "优先官方文档 · 标注版本号 · 交叉验证社区方案 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "优先官方文档 · 标注版本号 · 交叉验证社区方案 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "版本匹配 · 示例可运行 · 来源可追溯",
        simple: "版本匹配 · 示例可运行 · 来源可追溯",
        standard: "版本匹配 · 示例可运行 · 来源可追溯",
        hard: "逐项对照需求检查 → 版本匹配 → 示例可运行 → 来源可追溯。版本匹配 · 示例可运行 · 来源可追溯",
    },
};

/// `retrieval/code-retrieval`。
pub static RETRIEVAL_CODE_RETRIEVAL: Module = Module {
    label: "检索类代码检索",
    hint: "搜索开源实现参考，关注许可证与维护状态",
    understand_tail: "\n\n明确代码需求与约束 → 识别搜索关键词 → 评估许可证兼容性\n\n领域要点：搜索开源实现参考，关注许可证与维护状态",
    design: Tiered {
        trivial: "",
        simple: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        standard: "确定检索策略和关键来源 → 列出检索词与筛选标准。",
        hard: "确定检索策略和关键来源 → 列出检索词与筛选标准 → 设计去重与排序规则。\n→ 列出多个检索渠道并对比覆盖范围。",
    },
    execute: Tiered {
        trivial: "快速检索后 asymptotic-think_transition 进入 VERIFY。",
        simple: "搜索GitHub/npm/PyPI等 · 关注star数与更新频率 · 检查许可证 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "搜索GitHub/npm/PyPI等 · 关注star数与更新频率 · 检查许可证 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "搜索GitHub/npm/PyPI等 · 关注star数与更新频率 · 检查许可证 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "许可证兼容 · 维护活跃 · 代码质量可接受",
        simple: "许可证兼容 · 维护活跃 · 代码质量可接受",
        standard: "许可证兼容 · 维护活跃 · 代码质量可接受",
        hard: "逐项对照需求检查 → 许可证兼容 → 维护活跃 → 代码质量。许可证兼容 · 维护活跃 · 代码质量可接受",
    },
};

/// `analytics/data-analysis`。
pub static ANALYTICS_DATA_ANALYSIS: Module = Module {
    label: "分析类数据分析",
    hint: "明确指标与口径，清洗前置、结论可复现",
    understand_tail: "\n\n明确分析目标与指标 → 识别数据源与口径 → 评估数据质量\n\n领域要点：明确指标与口径，清洗前置、结论可复现",
    design: Tiered {
        trivial: "",
        simple: "确定分析方法和关键指标 → 列出数据源与处理步骤。",
        standard: "确定分析方法和关键指标 → 列出数据源与处理步骤。",
        hard: "确定分析方法和关键指标 → 列出数据源与处理步骤 → 设计验证方案。\n→ 列出多个分析角度并对比优劣。",
    },
    execute: Tiered {
        trivial: "快速分析后 asymptotic-think_transition 进入 VERIFY。",
        simple: "数据清洗前置 · 口径一致 · 中间结果可审查 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "数据清洗前置 · 口径一致 · 中间结果可审查 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "数据清洗前置 · 口径一致 · 中间结果可审查 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "结论有数据支撑 · 口径一致 · 分析过程可复现",
        simple: "结论有数据支撑 · 口径一致 · 分析过程可复现",
        standard: "结论有数据支撑 · 口径一致 · 分析过程可复现",
        hard: "逐项对照需求检查 → 结论有据 → 口径一致 → 可复现。结论有数据支撑 · 口径一致 · 分析过程可复现",
    },
};

/// `analytics/code-analysis`。
pub static ANALYTICS_CODE_ANALYSIS: Module = Module {
    label: "分析类代码分析",
    hint: "AST优先于文本匹配，分析结论标注置信度",
    understand_tail: "\n\n明确分析目标与代码范围 → 识别分析维度(结构/依赖/质量) → 评估工具选择\n\n领域要点：AST优先于文本匹配，分析结论标注置信度",
    design: Tiered {
        trivial: "",
        simple: "确定分析方法和关键维度 → 列出分析范围与工具。",
        standard: "确定分析方法和关键维度 → 列出分析范围与工具。",
        hard: "确定分析方法和关键维度 → 列出分析范围与工具 → 设计验证方案。\n→ 列出多个分析角度并对比优劣。",
    },
    execute: Tiered {
        trivial: "快速分析后 asymptotic-think_transition 进入 VERIFY。",
        simple: "AST解析优先 · 结论标注置信度 · 不确定处明示 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "AST解析优先 · 结论标注置信度 · 不确定处明示 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "AST解析优先 · 结论标注置信度 · 不确定处明示 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "分析覆盖完整 · 结论有依据 · 边界情况已考虑",
        simple: "分析覆盖完整 · 结论有依据 · 边界情况已考虑",
        standard: "分析覆盖完整 · 结论有依据 · 边界情况已考虑",
        hard: "逐项对照需求检查 → 覆盖完整 → 结论有据 → 边界已考虑。分析覆盖完整 · 结论有依据 · 边界情况已考虑",
    },
};

/// `analytics/log-analysis`。
pub static ANALYTICS_LOG_ANALYSIS: Module = Module {
    label: "分析类日志分析",
    hint: "时间线对齐、关联追踪，异常模式识别",
    understand_tail: "\n\n明确日志范围与时间窗口 → 识别关键字段与关联ID → 评估日志完整性\n\n领域要点：时间线对齐、关联追踪，异常模式识别",
    design: Tiered {
        trivial: "",
        simple: "确定分析方法和关键维度 → 列出日志范围与处理步骤。",
        standard: "确定分析方法和关键维度 → 列出日志范围与处理步骤。",
        hard: "确定分析方法和关键维度 → 列出日志范围与处理步骤 → 设计验证方案。\n→ 列出多个分析角度并对比优劣。",
    },
    execute: Tiered {
        trivial: "快速分析后 asymptotic-think_transition 进入 VERIFY。",
        simple: "时间线对齐 · 关联ID追踪 · 异常模式标注 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "时间线对齐 · 关联ID追踪 · 异常模式标注 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "时间线对齐 · 关联ID追踪 · 异常模式标注 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "根因定位准确 · 时间线完整 · 无遗漏关键日志",
        simple: "根因定位准确 · 时间线完整 · 无遗漏关键日志",
        standard: "根因定位准确 · 时间线完整 · 无遗漏关键日志",
        hard: "逐项对照需求检查 → 根因准确 → 时间线完整 → 无遗漏。根因定位准确 · 时间线完整 · 无遗漏关键日志",
    },
};

/// `analytics/requirement-analysis`。
pub static ANALYTICS_REQUIREMENT_ANALYSIS: Module = Module {
    label: "分析类需求分析",
    hint: "拆解需求粒度、明确验收标准、识别隐含约束",
    understand_tail: "\n\n拆解需求为可验证单元 → 明确验收标准 → 识别隐含约束与依赖\n\n领域要点：拆解需求粒度、明确验收标准、识别隐含约束",
    design: Tiered {
        trivial: "",
        simple: "确定分析方法和关键维度 → 列出需求范围与拆解方式。",
        standard: "确定分析方法和关键维度 → 列出需求范围与拆解方式。",
        hard: "确定分析方法和关键维度 → 列出需求范围与拆解方式 → 设计验证方案。\n→ 列出多个分析角度并对比优劣。",
    },
    execute: Tiered {
        trivial: "快速分析后 asymptotic-think_transition 进入 VERIFY。",
        simple: "需求条目化 · 验收标准可测试 · 优先级排序 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "需求条目化 · 验收标准可测试 · 优先级排序 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "需求条目化 · 验收标准可测试 · 优先级排序 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "需求覆盖完整 · 验收标准明确 · 无歧义",
        simple: "需求覆盖完整 · 验收标准明确 · 无歧义",
        standard: "需求覆盖完整 · 验收标准明确 · 无歧义",
        hard: "逐项对照需求检查 → 覆盖完整 → 标准明确 → 无歧义。需求覆盖完整 · 验收标准明确 · 无歧义",
    },
};

/// `devops/deploy`。
pub static DEVOPS_DEPLOY: Module = Module {
    label: "运维类部署上线",
    hint: "灰度发布、健康检查、回滚方案前置",
    understand_tail: "\n\n明确部署目标与环境 → 识别依赖服务与配置 → 评估回滚策略\n\n领域要点：灰度发布、健康检查、回滚方案前置",
    design: Tiered {
        trivial: "",
        simple: "确定部署步骤和验证方式 → 列出涉及服务与配置。",
        standard: "确定部署步骤和验证方式 → 列出涉及服务与配置。",
        hard: "确定部署步骤和验证方式 → 列出涉及服务与配置 → 设计回滚方案。\n→ 列出多个部署策略并对比风险。",
    },
    execute: Tiered {
        trivial: "快速部署后 asymptotic-think_transition 进入 VERIFY。",
        simple: "灰度/分批发布 · 健康检查验证 · 回滚方案就绪 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "灰度/分批发布 · 健康检查验证 · 回滚方案就绪 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "灰度/分批发布 · 健康检查验证 · 回滚方案就绪 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "服务健康 · 监控告警正常 · 回滚方案可执行",
        simple: "服务健康 · 监控告警正常 · 回滚方案可执行",
        standard: "服务健康 · 监控告警正常 · 回滚方案可执行",
        hard: "逐项对照需求检查 → 服务健康 → 告警正常 → 回滚可执行。服务健康 · 监控告警正常 · 回滚方案可执行",
    },
};

/// `devops/monitor`。
pub static DEVOPS_MONITOR: Module = Module {
    label: "运维类监控告警",
    hint: "指标-告警-通知链路完整，告警有处置SOP",
    understand_tail: "\n\n明确监控目标与指标 → 识别告警阈值与通知渠道 → 评估覆盖盲区\n\n领域要点：指标-告警-通知链路完整，告警有处置SOP",
    design: Tiered {
        trivial: "",
        simple: "确定监控指标和告警规则 → 列出通知渠道与处置流程。",
        standard: "确定监控指标和告警规则 → 列出通知渠道与处置流程。",
        hard: "确定监控指标和告警规则 → 列出通知渠道与处置流程 → 设计降级方案。\n→ 列出多个监控方案并对比覆盖度。",
    },
    execute: Tiered {
        trivial: "快速配置后 asymptotic-think_transition 进入 VERIFY。",
        simple: "指标采集完整 · 告警规则合理 · 通知链路验证 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "指标采集完整 · 告警规则合理 · 通知链路验证 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "指标采集完整 · 告警规则合理 · 通知链路验证 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "告警可触发 · 通知可达 · 处置SOP明确",
        simple: "告警可触发 · 通知可达 · 处置SOP明确",
        standard: "告警可触发 · 通知可达 · 处置SOP明确",
        hard: "逐项对照需求检查 → 告警可触发 → 通知可达 → SOP明确。告警可触发 · 通知可达 · 处置SOP明确",
    },
};

/// `devops/cicd`。
pub static DEVOPS_CICD: Module = Module {
    label: "运维类CI/CD",
    hint: "流水线幂等可重复，构建产物版本化",
    understand_tail: "\n\n明确流水线阶段与触发条件 → 识别构建依赖与环境 → 评估制品管理\n\n领域要点：流水线幂等可重复，构建产物版本化",
    design: Tiered {
        trivial: "",
        simple: "确定流水线阶段和触发条件 → 列出构建环境与制品。",
        standard: "确定流水线阶段和触发条件 → 列出构建环境与制品。",
        hard: "确定流水线阶段和触发条件 → 列出构建环境与制品 → 设计回滚方案。\n→ 列出多个流水线方案并对比效率。",
    },
    execute: Tiered {
        trivial: "快速配置后 asymptotic-think_transition 进入 VERIFY。",
        simple: "流水线幂等 · 构建产物版本化 · 失败快速反馈 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "流水线幂等 · 构建产物版本化 · 失败快速反馈 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "流水线幂等 · 构建产物版本化 · 失败快速反馈 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "流水线可重复执行 · 产物可追溯 · 测试全部通过",
        simple: "流水线可重复执行 · 产物可追溯 · 测试全部通过",
        standard: "流水线可重复执行 · 产物可追溯 · 测试全部通过",
        hard: "逐项对照需求检查 → 可重复 → 可追溯 → 测试通过。流水线可重复执行 · 产物可追溯 · 测试全部通过",
    },
};

/// `devops/config`。
pub static DEVOPS_CONFIG: Module = Module {
    label: "运维类环境配置",
    hint: "环境隔离、敏感信息加密、配置版本化",
    understand_tail: "\n\n明确配置范围与环境差异 → 识别敏感配置项 → 评估配置变更影响\n\n领域要点：环境隔离、敏感信息加密、配置版本化",
    design: Tiered {
        trivial: "",
        simple: "确定配置项和环境映射 → 列出敏感配置与加密方式。",
        standard: "确定配置项和环境映射 → 列出敏感配置与加密方式。",
        hard: "确定配置项和环境映射 → 列出敏感配置与加密方式 → 设计回滚方案。\n→ 列出多个配置管理方案并对比安全性。",
    },
    execute: Tiered {
        trivial: "快速配置后 asymptotic-think_transition 进入 VERIFY。",
        simple: "环境隔离 · 敏感信息加密存储 · 配置变更可追溯 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "环境隔离 · 敏感信息加密存储 · 配置变更可追溯 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "环境隔离 · 敏感信息加密存储 · 配置变更可追溯 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "环境配置一致 · 无硬编码敏感信息 · 配置版本可回滚",
        simple: "环境配置一致 · 无硬编码敏感信息 · 配置版本可回滚",
        standard: "环境配置一致 · 无硬编码敏感信息 · 配置版本可回滚",
        hard: "逐项对照需求检查 → 环境一致 → 无硬编码 → 可回滚。环境配置一致 · 无硬编码敏感信息 · 配置版本可回滚",
    },
};

/// `entertainment/fun-chat`。
pub static ENTERTAINMENT_FUN_CHAT: Module = Module {
    label: "娱乐类休闲聊天",
    hint: "风格一致、内容有趣、安全合规",
    understand_tail: "\n\n明确对话风格与边界 → 识别用户情绪与意图 → 评估安全合规要求\n\n领域要点：风格一致、内容有趣、安全合规",
    design: Tiered {
        trivial: "",
        simple: "确定对话风格和内容方向 → 列出安全边界。",
        standard: "确定对话风格和内容方向 → 列出安全边界。",
        hard: "确定对话风格和内容方向 → 列出安全边界 → 设计多轮互动方案。\n→ 列出多个风格方案并对比效果。",
    },
    execute: Tiered {
        trivial: "快速回应后 asymptotic-think_transition 进入 VERIFY。",
        simple: "风格一致 · 内容有趣不冒犯 · 安全边界内发挥 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "风格一致 · 内容有趣不冒犯 · 安全边界内发挥 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "风格一致 · 内容有趣不冒犯 · 安全边界内发挥 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "风格符合预期 · 无违规内容 · 用户意图满足",
        simple: "风格符合预期 · 无违规内容 · 用户意图满足",
        standard: "风格符合预期 · 无违规内容 · 用户意图满足",
        hard: "逐项对照需求检查 → 风格符合 → 无违规 → 意图满足。风格符合预期 · 无违规内容 · 用户意图满足",
    },
};

/// `entertainment/creative-writing`。
pub static ENTERTAINMENT_CREATIVE_WRITING: Module = Module {
    label: "娱乐类创意写作",
    hint: "风格一致、结构完整、原创性优先",
    understand_tail: "\n\n明确写作类型与风格 → 识别目标受众与目的 → 评估篇幅与结构要求\n\n领域要点：风格一致、结构完整、原创性优先",
    design: Tiered {
        trivial: "",
        simple: "确定写作风格和结构大纲 → 列出关键要素。",
        standard: "确定写作风格和结构大纲 → 列出关键要素。",
        hard: "确定写作风格和结构大纲 → 列出关键要素 → 设计多版本方案。\n→ 列出多个风格方案并对比效果。",
    },
    execute: Tiered {
        trivial: "快速创作后 asymptotic-think_transition 进入 VERIFY。",
        simple: "风格统一 · 结构完整 · 原创不抄袭 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "风格统一 · 结构完整 · 原创不抄袭 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "风格统一 · 结构完整 · 原创不抄袭 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "风格一致 · 结构完整 · 内容原创",
        simple: "风格一致 · 结构完整 · 内容原创",
        standard: "风格一致 · 结构完整 · 内容原创",
        hard: "逐项对照需求检查 → 风格一致 → 结构完整 → 内容原创。风格一致 · 结构完整 · 内容原创",
    },
};

/// `general/general`。
pub static GENERAL_GENERAL: Module = Module {
    label: "通用类通用",
    hint: "贴合上下文，保持目标导向",
    understand_tail: "\n\n明确任务目标与上下文 → 识别约束条件 → 评估所需资源\n\n领域要点：贴合上下文，保持目标导向",
    design: Tiered {
        trivial: "",
        simple: "确定实施路径和关键步骤 → 列出涉及资源与验收标准。",
        standard: "确定实施路径和关键步骤 → 列出涉及资源与验收标准。",
        hard: "确定实施路径和关键步骤 → 列出涉及资源与验收标准 → 设计备选方案。\n→ 列出多个方案并对比优劣。",
    },
    execute: Tiered {
        trivial: "快速执行后 asymptotic-think_transition 进入 VERIFY。",
        simple: "贴合上下文执行 · 保持目标导向 · 灵活调整 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        standard: "贴合上下文执行 · 保持目标导向 · 灵活调整 → 完成目标后 asymptotic-think_transition 进入 VERIFY。",
        hard: "贴合上下文执行 · 保持目标导向 · 灵活调整 · 每步验证结果 → 完成目标后 asymptotic-think_transition 进入 VERIFY。\n每步必须有明确验证，不可跳过",
    },
    verify: Tiered {
        trivial: "目标达成 · 上下文一致 · 无越界",
        simple: "目标达成 · 上下文一致 · 无越界",
        standard: "目标达成 · 上下文一致 · 无越界",
        hard: "逐项对照需求检查 → 目标达成 → 上下文一致 → 无越界。目标达成 · 上下文一致 · 无越界",
    },
};

/// `prompt-registry.ts` 的 kebab 路径 → 模块路由表。
pub static MODULES: &[(&str, &Module)] = &[
    ("coding/java-dev", &CODING_JAVA_DEV),
    ("coding/rust-dev", &CODING_RUST_DEV),
    ("coding/python-dev", &CODING_PYTHON_DEV),
    ("coding/js-dev", &CODING_JS_DEV),
    ("coding/go-dev", &CODING_GO_DEV),
    ("coding/crud-dev", &CODING_CRUD_DEV),
    ("coding/bug-fix", &CODING_BUG_FIX),
    ("coding/code-refactor", &CODING_CODE_REFACTOR),
    ("coding/testing", &CODING_TESTING),
    ("coding/architect", &CODING_ARCHITECT),
    ("coding/code-review", &CODING_CODE_REVIEW),
    ("coding/perf-optimize", &CODING_PERF_OPTIMIZE),
    ("retrieval/paper-retrieval", &RETRIEVAL_PAPER_RETRIEVAL),
    ("retrieval/daily-retrieval", &RETRIEVAL_DAILY_RETRIEVAL),
    ("retrieval/doc-retrieval", &RETRIEVAL_DOC_RETRIEVAL),
    ("retrieval/code-retrieval", &RETRIEVAL_CODE_RETRIEVAL),
    ("analytics/data-analysis", &ANALYTICS_DATA_ANALYSIS),
    ("analytics/code-analysis", &ANALYTICS_CODE_ANALYSIS),
    ("analytics/log-analysis", &ANALYTICS_LOG_ANALYSIS),
    ("analytics/requirement-analysis", &ANALYTICS_REQUIREMENT_ANALYSIS),
    ("devops/deploy", &DEVOPS_DEPLOY),
    ("devops/monitor", &DEVOPS_MONITOR),
    ("devops/cicd", &DEVOPS_CICD),
    ("devops/config", &DEVOPS_CONFIG),
    ("entertainment/fun-chat", &ENTERTAINMENT_FUN_CHAT),
    ("entertainment/creative-writing", &ENTERTAINMENT_CREATIVE_WRITING),
    ("general/general", &GENERAL_GENERAL),
];
