# Frontend foundations

Shared frontend primitives for the Desktop and hosted Web products. Import an
explicit subpath such as `@centaeris/frontend-foundations/button`,
`@centaeris/frontend-foundations/primitives/menu` or
`@centaeris/frontend-foundations/theme-store`.

Nova components contain their existing Tailwind styles; consumers using them
must include this package's source in their Tailwind scan. Headless Base UI
exports allow consumers to retain their own markup and CSS. Importing JavaScript
does not install a global CSS reset. Theme declarations are an explicit CSS import.

Product routes, layout policy, transcript folding, follow mode, request ownership
and execution semantics stay in their existing owners. See
[Frontend foundations](../../docs/development/FrontendFoundations.md) for the
shared boundary and validation requirements.

The Nova components are adapted from shadcn/ui under MIT; preserve the file
headers and [third-party notice](../../THIRD_PARTY_NOTICES.md).
