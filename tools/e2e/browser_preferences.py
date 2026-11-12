"""Shared browser actions for the appearance picker."""


def select_theme(page, theme: str) -> None:
    page.locator('#btn-theme').click()
    page.locator(f'[data-theme-choice="{theme}"]').click()


def toggle_theme(page) -> None:
    """Keep existing dark/light comparison probes using the real selection UI."""
    current = page.locator('html').get_attribute('data-theme')
    select_theme(page, 'light' if current == 'dark' else 'dark')
