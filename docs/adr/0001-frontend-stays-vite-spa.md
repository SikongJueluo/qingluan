# 前端保持 Vite SPA，不引入 Nuxt

在规划 code review 界面时评估过是否升级到 Nuxt，结论是保持现有的
Vite + Vue SPA 架构。原因：前端产物作为静态资源内嵌进 Tauri webview，
或作为纯客户端 SPA 访问本地 daemon（127.0.0.1:47129），Nuxt 的核心价值
（SSR、SEO、Nitro server routes）在这两种形态下都用不上；同时项目使用
Vue beta（Vapor mode），Nuxt 对其兼容性有风险，迁移路由与构建配置的
成本是纯负收益。

**重新评估的时机**：只有当 qingluan 出现公网多用户的 SaaS 形态（需要登录、
分享链接的 SEO/OG preview、服务端鉴权）时才值得重提。
