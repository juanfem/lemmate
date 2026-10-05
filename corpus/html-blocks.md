---
title: HTML blocks
format: revealjs
---

## A slide whose figure follows an HTML line

::: {.columns}
::: {.column width="48%"}
- A point with [a link](https://example.org/a)

<p class="xref">After <i>somebody</i>, <a href="https://example.org/b">source</a></p>
:::
::: {.column width="52%"}
![](figures/plot.png)
:::
:::

<div class="note">
See [the notes](notes/detail.md) and <img src="figures/inline.png" alt="x">.
</div>

Inline <img SRC = 'figures/quoted.png'> and <img src=bare.png> in a paragraph.

<!-- ![](figures/commented-out.png) -->

<img srcset="figures/no.png 2x" data-src="figures/neither.png">

> <p>quoted</p>
> ![](figures/in-quote.png)

```{=html}
<img src="figures/raw-code.png">
```
