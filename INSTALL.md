## Install

```bash
curl -sSfL https://tell.rs/agent | bash
```

### Cloud

```bash
witness setup --token YOUR_API_KEY
```

### Self-hosted

```bash
witness setup --token YOUR_API_KEY --server http://your-server:8080
```

The agent fetches its config from your Tell server. No internet needed after install.

### Air-gapped

Pre-stage the binary via internal mirror, USB, or config management. Then:

```bash
witness setup --token YOUR_API_KEY --endpoint your-server:50000
```

Or write `/etc/tell/agent.toml` directly:

```toml
api_key = "your-api-key"
endpoint = "your-server:50000"
```

Then: `systemctl enable --now witness`

### Configuration

Default config works out of the box. To customise log paths, tags, or device filters see [configs/example.toml](configs/example.toml). Reload without downtime: `systemctl reload witness`
