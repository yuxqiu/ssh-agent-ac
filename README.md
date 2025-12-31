# ssh-agent-ac

A thin wrapper around OpenSSH's `ssh-agent` that enforces confirmation for every use of all added SSH keys.

**Why**: Requiring confirmation for each use of a key stored in `ssh-agent` helps defend against [agent hijacking](https://embracethered.com/blog/posts/2022/ttp-diaries-ssh-agent-hijacking). Normally, confirmation can be enabled only at add time with `ssh-add -c <key>`. This is not possible when keys are added externally (e.g., by password managers or authentication tools). `ssh-agent-ac` solves this by forcing confirmation on every key operation regardless of how the key was added. It achieves this by wrapping the standard `ssh-agent`, intercepting requests to add keys, and re-issuing them with the equivalent of the `-c` flag.

## Installation

### From this flake

```bash
nix run github:yuxqiu/ssh-agent-ac
```

To integrate with Home Manager, see [my ssh-agent-ac module](https://github.com/yuxqiu/.dotfiles/blob/main/nix/hm/common/ssh/ssh-agent-ac.nix).

### Release

Pre-built binaries are available on the [Releases](https://github.com/yuxqiu/ssh-agent-ac/releases) page.

### Manual build

```bash
cargo build --release
sudo cp target/release/ssh-agent-ac /usr/local/bin/
```

## Usage

Start the agent:

```bash
export SSH_AUTH_SOCK=/tmp/ssh-agent-ac.sock
ssh-agent-ac -s /tmp/ssh-agent-ac.sock
```

For additional options:

```bash
ssh-agent-ac --help
```

## License

[MIT License](./LICENSE)
