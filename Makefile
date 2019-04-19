.PHONY: install-hooks
.PHONY: clean

clean:
	cargo clean
	rm -rf venv

.git/hooks/pre-commit: venv
	${CURDIR}/venv/bin/pre-commit install --install-hooks
	cargo fmt --help > /dev/null || rustup component add rustfmt
	cargo clippy --help > /dev/null || rustup component add clippy

install-hooks: .git/hooks/pre-commit
	@true

venv:
	virtualenv venv
	./venv/bin/pip install pre-commit
