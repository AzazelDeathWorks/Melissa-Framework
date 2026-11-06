// Compose creates only the exact readonly/none-network keeper. The existing
// executor still injects the snapshot, attests kernel policy, owns the deadline,
// captures evidence and removes the single nonce/image-bound container by ID.
fn prepare_compose_client(docker:&mut Docker,backend:CreationBackend)->ExecutorResult<Option<(File,Vec<File>)>> {
    if backend!=CreationBackend::Compose{return Ok(None)}
    #[cfg(windows)] {
        let plugin=std::path::Path::new(r"C:\Program Files\Docker\Docker\resources\cli-plugins\docker-compose.exe");
        // Docker Desktop may hardlink its bundled plugin. Hold a read-only
        // source handle and every ancestor while copying; execute only the
        // verified, single-link private copy, pinned against write/replacement.
        let(mut source,_source_ancestors,source_identity)=pin_client_mode(plugin,false)?;
        std::io::Seek::rewind(&mut source)?;
        let directory=docker._config.path().join("cli-plugins");std::fs::create_dir(&directory)?;
        let copy=directory.join("docker-compose.exe");
        let mut output=OpenOptions::new().create_new(true).write(true).open(&copy)?;
        std::io::copy(&mut source,&mut output)?;output.sync_all()?;drop(output);
        let (file,ancestors,identity)=pin_client(&copy)?;
        if identity.rsplit(':').next()!=source_identity.rsplit(':').next(){return Err(fail("Compose copy differs from pinned installation bytes"))}
        std::fs::write(docker._config.path().join("config.json"),serde_json::to_vec(&serde_json::json!({"cliPluginsExtraDirs":[directory]}))?)?;
        docker.client_identity.push_str(&format!(":compose-source:{source_identity}:private:{identity}"));
        Ok(Some((file,ancestors)))
    }
    #[cfg(not(windows))] { let _=docker; Err(ExecutorError::Unsupported("Compose uses the pinned Windows Docker plugin".into())) }
}
fn compose_manifest(request:&ContainerRequest,name:&str,nonce:&str,deadline:u64)->Value{
    let l=&request.limits;
    serde_json::json!({"services":{"worker":{
        "image":request.image_id,"pull_policy":"never","container_name":name,
        "labels":{"melissa.code03.nonce":nonce,"melissa.code03.contract":EXECUTION_CONTRACT},
        "entrypoint":["/usr/bin/perl"],"command":["-e",KEEPER.replace('$',"$$"),"--",deadline.to_string()],
        "environment":controlled_environment(),"user":"65533:65533","read_only":true,
        "network_mode":"none","ipc":"none","cap_drop":["ALL"],"security_opt":["no-new-privileges:true"],
        "logging":{"driver":"none"},"healthcheck":{"disable":true},"stop_grace_period":"0s",
        "mem_limit":l.memory_bytes,"memswap_limit":l.memory_bytes,"pids_limit":l.max_processes,
        "cpu_period":1000000,"cpu_quota":u64::from(l.cpu_rate_basis_points)*100,
        "ulimits":{"fsize":{"soft":l.disk_bytes,"hard":l.disk_bytes}},
        "tmpfs":[format!("/work:rw,exec,nosuid,nodev,size={},nr_inodes=8192,uid=65534,gid=65534,mode=0700",l.disk_bytes)],
        "restart":"no"}},"networks":{}})
}
fn compose_create(docker:&Docker,request:&ContainerRequest,name:&str,nonce:&str,deadline:u64)->ExecutorResult<String>{
    let path=docker._config.path().join("compose.json");
    std::fs::write(&path,serde_json::to_vec(&compose_manifest(request,name,nonce,deadline))?)?;
    docker.checked(&strings(&["compose","--project-name",name,"--project-directory",docker._config.path().to_str().ok_or_else(||fail("Compose directory encoding"))?,"--file",path.to_str().ok_or_else(||fail("Compose file encoding"))?,"create","--no-build","--pull","never","worker"]),None,CONTROL_LIMIT)?;
    let inspected=docker.inspect(name)?;
    if inspected["Config"]["Labels"]["com.docker.compose.project"]!=name || inspected["Config"]["Labels"]["com.docker.compose.service"]!="worker" {
        return Err(fail("Compose returned a foreign project/service"))
    }
    inspected["Id"].as_str().map(str::to_string).ok_or_else(||fail("Compose container identity unavailable"))
}
