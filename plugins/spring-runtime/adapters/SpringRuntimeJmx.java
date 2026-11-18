import java.util.*;
import javax.management.*;
import javax.management.openmbean.CompositeData;
import javax.management.remote.*;

/** Read-only JMX probe: localhost only, no credentials. Reads fixed attributes and invokes only the Spring Boot health and info operations. */
public class SpringRuntimeJmx {
    static final String[][] ALLOWED = {
        {"java.lang:type=Runtime", "Uptime", "VmName", "VmVersion", "InputArguments"},
        {"java.lang:type=Memory", "HeapMemoryUsage"},
        {"java.lang:type=Threading", "ThreadCount"},
        {"java.lang:type=OperatingSystem", "SystemLoadAverage", "AvailableProcessors"},
    };
    static final String[][] SPRING = {
        {"org.springframework.boot:type=Endpoint,name=Health", "health"},
        {"org.springframework.boot:type=Endpoint,name=Info", "info"},
    };

    static String esc(String s) {
        StringBuilder b = new StringBuilder("\"");
        for (char c : (s.length() > 400 ? s.substring(0, 400) : s).toCharArray()) {
            if (c == '"' || c == '\\') b.append('\\').append(c);
            else if (c < 0x20) b.append(String.format("\\u%04x", (int) c));
            else b.append(c);
        }
        return b.append('"').toString();
    }

    static String value(Object v) {
        if (v instanceof CompositeData d) {
            return "used=" + d.get("used") + " committed=" + d.get("committed") + " max=" + d.get("max");
        }
        if (v instanceof Object[] a) {
            StringBuilder b = new StringBuilder("[");
            for (int i = 0; i < Math.min(a.length, 64); i++) b.append(i > 0 ? "," : "").append(esc(String.valueOf(a[i])));
            return b.append("]").toString();
        }
        return esc(String.valueOf(v));
    }

    static void add(StringBuilder out, MBeanServerConnection c, String bean, String attr) {
        try {
            Object v = c.getAttribute(new ObjectName(bean), attr);
            out.append(out.length() > 1 ? "," : "").append(esc(bean + "#" + attr)).append(':')
               .append(v instanceof Object[] ? value(v) : esc(v instanceof CompositeData ? value(v) : String.valueOf(v)));
        } catch (Exception e) {
        }
    }

    /** One output entry per leaf (`bean#op().path.to.leaf`), so the plugin can mask by the leaf key. */
    static void flatten(StringBuilder out, String key, Object v, int depth) {
        if (out.length() > 20000 || v == null) return;
        if ((v instanceof Map<?, ?> || v instanceof Iterable<?>) && depth >= 6) {
            out.append(out.length() > 1 ? "," : "").append(esc(key)).append(':').append(esc("(nested too deep, not shown)"));
        } else if (v instanceof Map<?, ?> m) {
            for (Map.Entry<?, ?> e : m.entrySet()) flatten(out, key + "." + e.getKey(), e.getValue(), depth + 1);
        } else if (v instanceof Iterable<?> items) {
            int i = 0;
            for (Object item : items) {
                if (i >= 64) break;
                flatten(out, key + "[" + i + "]", item, depth + 1);
                i++;
            }
        } else if (v instanceof String || v instanceof Number || v instanceof Boolean || v instanceof Enum<?>) {
            out.append(out.length() > 1 ? "," : "").append(esc(key)).append(':').append(esc(String.valueOf(v)));
        } else {
            out.append(out.length() > 1 ? "," : "").append(esc(key)).append(':').append(esc("(value not shown)"));
        }
    }

    static void operation(StringBuilder out, MBeanServerConnection c, String bean, String name) {
        try {
            Object v = c.invoke(new ObjectName(bean), name, new Object[0], new String[0]);
            flatten(out, bean + "#" + name + "()", v, 0);
        } catch (Exception e) {
        }
    }

    public static void main(String[] args) throws Exception {
        int port = args.length == 1 ? Integer.parseInt(args[0]) : 0;
        if (port < 1024 || port > 65535) {
            System.err.println("port must be 1024-65535");
            System.exit(2);
        }
        JMXServiceURL url = new JMXServiceURL("service:jmx:rmi:///jndi/rmi://localhost:" + port + "/jmxrmi");
        StringBuilder out = new StringBuilder("{");
        try (JMXConnector connector = JMXConnectorFactory.connect(url)) {
            MBeanServerConnection c = connector.getMBeanServerConnection();
            for (String[] entry : ALLOWED) {
                for (int i = 1; i < entry.length; i++) add(out, c, entry[0], entry[i]);
            }
            for (String[] entry : SPRING) {
                if (c.isRegistered(new ObjectName(entry[0]))) operation(out, c, entry[0], entry[1]);
            }
        }
        System.out.println(out.append('}'));
    }
}
